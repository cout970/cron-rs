pub mod dayofweek;
pub mod file;
pub mod logging;
pub mod shorthand;
pub mod timeunit;
pub mod validation;

use anyhow::{anyhow, bail, Context, Result};
use chrono::TimeZone;
use chrono_tz::{Tz, UTC};
use nom::branch::alt;
use nom::bytes::complete::tag;
use nom::character::complete::{digit1, multispace0, space0};
use nom::combinator::{all_consuming, map, map_res, opt, value};
use nom::error::ParseError;
use nom::multi::separated_list1;
use nom::sequence::{delimited, preceded, separated_pair, tuple};
use nom::{AsChar, IResult, InputTakeAtPosition, Parser};

use self::dayofweek::DayOfWeek;
use self::file::ExplodedTimePatternFieldConfig;
use self::file::{ConfigFile, ExplodedTimePatternConfig, TaskDefinition, TimePatternConfig};
use self::logging::LoggingConfig;
use self::timeunit::TimeUnit;

use log::warn;
use std::collections::HashMap;
use std::time::Duration;
use crate::alerts::AlertConfig;

#[derive(Debug, Clone)]
pub struct TaskConfig {
    pub name: String,
    pub cmd: String,
    pub schedule: Schedule,
    pub timezone: Tz,
    pub avoid_overlapping: bool,
    pub run_as: Option<String>,
    pub time_limit: Option<u64>,
    pub working_directory: Option<String>,
    pub env: Option<HashMap<String, String>>,
    pub shell: Option<String>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub tasks: Vec<TaskConfig>,
    pub logging: LoggingConfig,
    pub alerts: AlertConfig,
}

#[derive(Debug, Clone)]
pub enum Schedule {
    Every { interval: Duration },
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
    let mut tasks: Vec<TaskConfig> = Vec::with_capacity(file.tasks.len());

    for (i, config) in file.tasks.iter().enumerate() {
        let task = TaskConfig::parse(config).context(format!(
            "Malformed task '{}' at position {}",
            &config.name,
            i + 1
        ))?;
        tasks.push(task);
    }

    Ok(Config {
        tasks,
        logging: file.logging.clone().unwrap_or_default(),
        alerts: file.alerts.clone().unwrap_or_default(),
    })
}

impl TaskConfig {
    fn parse(config: &TaskDefinition) -> Result<Self> {
        if config.when.is_some() && config.every.is_some() {
            bail!(
                "Task '{}' defines both 'when' and 'every'. Only one is allowed.",
                config.name
            );
        }

        let schedule = if let Some(when) = &config.when {
            Schedule::parse_when(when)?
        } else if let Some(every) = &config.every {
            Schedule::parse_every(every.as_str())?
        } else {
            bail!("No schedule specified for task '{}'", config.name);
        };

        let timezone: Tz = if let Some(timezone_name) = &config.timezone {
            timezone_name.parse()?
        } else {
            iana_time_zone::get_timezone()
                .expect("Unable to get system timezone")
                .parse()?
        };

        let time_limit = if let Some(def) = &config.time_limit {
            let duration = Schedule::parse_time_duration(def)?;
            if duration.as_secs() < 1 {
                warn!("Task '{}': cannot have a time limit of less than 1 second. Changed to 1 second", config.name);
            }
            Some(duration.as_secs().max(1))
        } else {
            None
        };

        Ok(Self {
            name: config.name.clone(),
            cmd: config.cmd.clone(),
            schedule,
            timezone,
            avoid_overlapping: config.avoid_overlapping,
            run_as: config.run_as.clone(),
            time_limit,
            shell: config.shell.clone(),
            working_directory: config.working_directory.clone(),
            env: config.env.clone(),
            stdout: config.stdout.clone(),
            stderr: config.stderr.clone(),
        })
    }
}

impl Schedule {
    fn parse_time_duration(input: &str) -> Result<Duration> {
        let amount_unit = separated_pair(number, space0, TimeUnit::parse);
        let line = delimited(space0, amount_unit, space0);

        let result = all_consuming(line)(input);

        let (amount, unit) = result.map_err(|e| anyhow!("Failed to parse: {}", e))?.1;

        Ok(unit.to_duration(amount))
    }

    fn parse_every(input: &str) -> Result<Self> {
        Ok(Self::Every {
            interval: Self::parse_time_duration(input)?,
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
        fn field_second(
            opt: &Option<ExplodedTimePatternFieldConfig>,
            allow_dow: bool,
        ) -> Result<TimePatternField> {
            if let Some(field) = opt {
                TimePatternField::parse_exploded_field(field, allow_dow)
            } else {
                Ok(TimePatternField::Value(0))
            }
        }

        Ok(TimePattern {
            year: field(&config.year, false).context("Malformed field: year")?,
            month: field(&config.month, false).context("Malformed field: month")?,
            day: field(&config.day, false).context("Malformed field: day")?,
            hour: field(&config.hour, false).context("Malformed field: hour")?,
            minute: field(&config.minute, false).context("Malformed field: minute")?,
            second: field_second(&config.second, false).context("Malformed field: second")?,
            day_of_week: field(&config.day_of_week, true)
                .context("Malformed field: day_of_week")?,
        })
    }
}

impl TimePatternField {
    /// Checks if the field matches a given value
    pub fn matches_value(&self, value: u32) -> bool {
        match self {
            TimePatternField::Any => true,
            TimePatternField::Value(v) => value == *v,
            TimePatternField::Range(start, end) => value >= *start && value <= *end,
            TimePatternField::List(values) => values.contains(&value),
            TimePatternField::Ratio(divisor, offset) => value % divisor + *offset == 0,
        }
    }
    
    /// Returns a tuple with the next valid value and 1 if the value requires wrapping, 0 if it doesn't
    pub fn get_next_valid_value(&self, the_value: u32, limit: u32) -> (u32, u32) {
        let value = (the_value + limit) % limit;
        match self {
            TimePatternField::Any => (value, 0),
            TimePatternField::Value(v) => {
                if value <= *v {
                    (*v, 0)
                } else {
                    (*v, 1)
                }
            }
            TimePatternField::Range(start, end) => {
                if value < *start {
                    (*start, 0)
                } else if value > *end {
                    (*start, 1)
                } else {
                    (value, 0)
                }
            }
            TimePatternField::List(values) => {
                if let Some(next_value) = values.iter().find(|&&v| v >= value) {
                    if value <= *next_value {
                        (*next_value, 0)
                    } else {
                        (*next_value, 1)
                    }
                } else {
                    // If no value is found, return the first value in the list
                    (*values.first().unwrap_or(&value), 1)
                }
            }
            TimePatternField::Ratio(divisor, offset) => {
                let mut curr = value;
                let mut rest = 0u32;

                // Do a full cycle to find the next valid value
                for i in 0..limit {
                    if curr % divisor + *offset == 0 {
                        return (curr, rest);
                    }
                    if curr + 1 >= limit {
                        rest = 1;
                    }
                    curr = (curr + 1) % limit;
                }

                // No value matches the pattern, return the current value
                (value, rest)
            }
        }
    }
    
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
    alt((number, map(DayOfWeek::parse, DayOfWeek::to_u32)))(i)
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
