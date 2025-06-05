pub mod dayofweek;
pub mod file;
pub mod logging;
pub mod shorthand;
pub mod timeunit;
pub mod validation;

use anyhow::{anyhow, bail, Context, Result};
use nom::branch::alt;
use nom::bytes::complete::tag;
use nom::character::complete::{digit1, multispace0, space0};
use nom::combinator::{all_consuming, map, map_res, opt, value};
use nom::error::ParseError;
use nom::multi::separated_list1;
use nom::sequence::{delimited, preceded, separated_pair, tuple};
use nom::{AsChar, IResult, InputTakeAtPosition, Parser};
use chrono::{TimeZone};
use chrono_tz::{Tz, UTC};

use self::dayofweek::DayOfWeek;
use self::file::ExplodedTimePatternFieldConfig;
use self::file::{ConfigFile, ExplodedTimePatternConfig, TaskConfig, TimePatternConfig};
use self::timeunit::TimeUnit;
use self::logging::LoggingConfig;

use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Config {
    pub tasks: Vec<Task>,
    pub logging: LoggingConfig,
}

#[derive(Debug, Clone)]
pub struct Task {
    pub name: String,
    pub cmd: String,
    pub timezone: Tz,
    pub schedule: Schedule,
}

#[derive(Debug, Clone)]
pub enum Schedule {
    Every { interval: std::time::Duration },
    When { time: TimePattern },
}

#[derive(Debug, Clone)]
pub struct TimePattern {
    pub second: TimePatternField,
    pub minute: TimePatternField,
    pub hour: TimePatternField,
    pub day_of_week: TimePatternField,
    pub day: TimePatternField,
    pub month: TimePatternField,
    pub year: TimePatternField,
}

// OnCalendar=[Mon,Tue] *-*/2-01..04 12:00:00

#[derive(Debug, Clone)]
pub enum TimePatternField {
    Any,             // * or missing
    Value(u32),      // 12
    Range(u32, u32), // 01..04
    List(Vec<u32>),  // [Mon,Tue]
    Ratio(u32, u32), // */5+2
}

pub fn parse_config_file(file: &ConfigFile) -> Result<Config> {
    let mut tasks: Vec<Task> = Vec::with_capacity(file.tasks.len());

    for (i, config) in file.tasks.iter().enumerate() {
        let task = Task::parse(config).context(format!(
            "Malformed task '{}' at position {}",
            &config.name,
            i + 1
        ))?;
        tasks.push(task);
    }

    Ok(Config { 
        tasks,
        logging: file.logging.clone().unwrap_or_default(),
    })
}

impl Task {
    fn parse(config: &TaskConfig) -> Result<Self> {
        if config.when.is_some() && config.every.is_some() {
            bail!(
                "Task '{}' defines both 'when' and 'every'. Only one is allowed.",
                config.name
            );
        }

        let schedule = if let Some(when) = &config.when {
            Schedule::parse_when(when)?
        } else if let Some(every) = &config.every {
            Schedule::parse_every(every.clone())?
        } else {
            bail!("No schedule specified for task '{}'", config.name);
        };

        let timezone: Tz = if let Some(timezone_name) = &config.timezone {
            timezone_name.parse()?
        } else {
            iana_time_zone::get_timezone().expect("Unable to get system timezone").parse()?
        };

        Ok(Self {
            name: config.name.clone(),
            cmd: config.cmd.clone(),
            timezone,
            schedule,
        })
    }
}

impl Schedule {
    fn parse_every(config: String) -> Result<Self> {
        let input = config.as_str();

        let amount_unit = separated_pair(number, space0, TimeUnit::parse);
        let line = delimited(space0, amount_unit, space0);

        let result = all_consuming(line)(input);

        let (amount, unit) = result.map_err(|e| anyhow!("Failed to parse: {}", e))?.1;

        Ok(Self::Every {
            interval: unit.to_duration(amount),
        })
    }

    fn parse_when(config: &TimePatternConfig) -> Result<Self> {
        let time = match config {
            TimePatternConfig::Short(s) => TimePattern::parse_short(s)?,
            TimePatternConfig::Long(c) => TimePattern::parse_long(c)?,
        };
        Ok(Schedule::When { time })
    }
}

impl TimePattern {
    fn parse_short(config: &String) -> Result<Self> {
        shorthand::parse_shorthand(config)
    }

    fn parse_long(config: &ExplodedTimePatternConfig) -> Result<Self> {
        fn field(
            opt: &Option<ExplodedTimePatternFieldConfig>,
            allow_dow: bool,
        ) -> Result<TimePatternField> {
            if let Some(field) = opt {
                TimePatternField::parse_exploded_field(field, allow_dow)
            } else {
                Ok(TimePatternField::Any)
            }
        }

        Ok(TimePattern {
            year: field(&config.year, false).context("Malformed field: year")?,
            month: field(&config.month, false).context("Malformed field: month")?,
            day: field(&config.day, false).context("Malformed field: day")?,
            hour: field(&config.hour, false).context("Malformed field: hour")?,
            minute: field(&config.minute, false).context("Malformed field: minute")?,
            second: field(&config.second, false).context("Malformed field: second")?,
            day_of_week: field(&config.day_of_week, true)
                .context("Malformed field: day_of_week")?,
        })
    }
}

impl TimePatternField {
    pub fn parse_exploded_field(
        config: &ExplodedTimePatternFieldConfig,
        allow_dow: bool,
    ) -> Result<Self> {
        match config {
            ExplodedTimePatternFieldConfig::Number(n) => Ok(TimePatternField::Value(*n)),
            ExplodedTimePatternFieldConfig::Text(s) => {
                Self::parse_exploded_text_field(s, allow_dow)
            }
            ExplodedTimePatternFieldConfig::List(list) => {
                Self::parse_exploded_list_field(list, allow_dow)
            }
        }
    }

    fn parse_exploded_list_field(input: &Vec<String>, allow_dow: bool) -> Result<Self> {
        let mut output: Vec<u32> = Vec::with_capacity(input.len());
        for s in input {
            let res = all_consuming(ws(time_atom(allow_dow)))(s);
            let (_, n) = res.map_err(|e| anyhow!("{}", e))?;
            output.push(n);
        }
        Ok(TimePatternField::List(output))
    }

    fn parse_exploded_text_field(i: &str, allow_dow: bool) -> Result<Self> {
        let res = all_consuming(shorthand::single_field(allow_dow))(i);
        let (_, field) = res.map_err(|e| anyhow!("{}", e))?;
        Ok(field)
    }
}

fn number(input: &str) -> IResult<&str, u32> {
    map_res(digit1, |s| str::parse::<u32>(s))(input)
}

fn number_or_daw(i: &str) -> IResult<&str, u32> {
    alt((ws(map(DayOfWeek::parse, DayOfWeek::to_u32)), ws(number)))(i)
}

fn time_atom<'a>(allow_dow: bool) -> impl FnMut(&'a str) -> IResult<&'a str, u32> {
    match allow_dow {
        true => number_or_daw,
        false => number,
    }
}

pub fn ws<I, O, E: ParseError<I>, F>(inner: F) -> impl FnMut(I) -> IResult<I, O, E>
where
    F: Parser<I, O, E>,
    I: InputTakeAtPosition,
    <I as InputTakeAtPosition>::Item: AsChar + Clone,
{
    delimited(multispace0, inner, multispace0)
}
