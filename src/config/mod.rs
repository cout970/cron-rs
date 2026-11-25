pub mod cron;
pub mod dayofweek;
pub mod file;
pub mod loader;
pub mod logging;
pub mod shorthand;
pub mod timeunit;
pub mod validation;
pub mod watchdog;

use anyhow::{anyhow, bail, Context, Result};
use chrono::{Datelike, NaiveDate, TimeZone};
use chrono_tz::{Tz, UTC};
use nom::branch::alt;
use nom::bytes::complete::tag;
use nom::character::complete::{digit1, multispace0, space0};
use nom::combinator::{all_consuming, map, map_res, opt, value};
use nom::error::ParseError;
use nom::multi::separated_list1;
use nom::sequence::{delimited, preceded, separated_pair, tuple};
use nom::{error, AsChar, IResult, InputTakeAtPosition, Parser};

use self::dayofweek::DayOfWeek;
use self::file::ExplodedTimePatternFieldConfig;
use self::file::{ConfigFile, ExplodedTimePatternConfig, TaskDefinition, TimePatternConfig};
use self::logging::LoggingConfig;
use self::timeunit::TimeUnit;
use self::watchdog::WatchdogConfig;

use log::warn;
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use std::time::Duration;
use crate::alerts::{Alert, AlertConfig};
use crate::sqlite_logger::SqliteLoggerConfig;

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
    pub on_failure: Vec<Alert>,
    pub on_success: Vec<Alert>,
}

#[derive(Debug, Clone, Default)]
pub struct Config {
    pub tasks: Vec<Arc<TaskConfig>>,
    pub logging: LoggingConfig,
    pub alerts: AlertConfig,
    pub watchdog: WatchdogConfig,
}

#[derive(Debug, Clone)]
pub enum Schedule {
    Every { interval: Duration, aligned: bool },
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
    Range(u32, u32), // 01..04 or 01..=04
    List(Vec<u32>),  // [Mon,Tue]
    Ratio(u32, u32), // */5+2
}

pub fn parse_config_file(file: &ConfigFile) -> Result<Config> {
    let mut tasks: Vec<Arc<TaskConfig>> = Vec::with_capacity(file.tasks.len());

    for (i, config) in file.tasks.iter().enumerate() {
        let task = TaskConfig::parse(config).context(format!(
            "Malformed task '{}' at position {}",
            &config.name,
            i + 1
        ))?;
        tasks.push(Arc::new(task));
    }

    let logging_config = file.logging.clone().unwrap_or_default();

    Ok(Config {
        tasks,
        logging: logging_config,
        alerts: file.alerts.clone().unwrap_or_default(),
        watchdog: file.watchdog.clone().unwrap_or_default(),
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
            let duration = Schedule::parse_time_duration(def)?.0;
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
            on_failure: config.on_failure.clone(),
            on_success: config.on_success.clone()
        })
    }
}

impl Schedule {
    fn parse_time_duration(input: &str) -> Result<(Duration, bool)> {
        pub fn parse_line<'s>() -> impl FnMut(&'s str) -> IResult<&'s str, (u32, TimeUnit, bool), error::Error<&'s str>>
        {
            move |input: &str| {
                let (input, _) = space0.parse(input)?;
                let (input, o2) = separated_pair(number, space0, TimeUnit::parse).parse(input)?;
                let (input, _) = space0.parse(input)?;
                let (input, o3) = opt(tag("aligned")).parse(input)?;

                Ok((input, (o2.0, o2.1, o3.is_some())))
            }
        }

        let result = all_consuming(parse_line())(input);

        let (amount, unit, aligned) = result.map_err(|e| anyhow!("Failed to parse: {}", e))?.1;
        let interval = unit.to_duration(amount);

        Ok((interval, aligned))
    }

    fn parse_every(input: &str) -> Result<Self> {
        let (interval, aligned) = Self::parse_time_duration(input)?;
        Ok(Self::Every { interval, aligned })
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

impl TimePattern {
    /// Checks the date fields (year, month, day, day of week) against a local date.
    /// Month and day are 1-based (January = 1, first day of the month = 1),
    /// day of week follows cron: Sunday = 0 (or 7), Monday = 1, ..., Saturday = 6.
    pub fn matches_date(&self, date: NaiveDate) -> bool {
        self.year.matches_value(date.year().max(0) as u32)
            && self.month.matches_one_based(date.month())
            && self.day.matches_one_based(date.day())
            && self.matches_day_of_week(date.weekday().num_days_from_sunday())
    }

    fn matches_day_of_week(&self, dow: u32) -> bool {
        self.day_of_week.matches_value(dow) || (dow == 0 && self.day_of_week.matches_value(7))
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
            // '*/5+1' matches 1, 6, 11, ...
            TimePatternField::Ratio(divisor, offset) => {
                *divisor != 0 && value >= *offset && (value - offset) % divisor == 0
            }
        }
    }

    /// Like `matches_value` for 1-based fields (day, month). Explicit values are 1-based,
    /// ratios count from the first value: '*/2' matches days 1, 3, 5... and months Jan, Mar, May...
    pub fn matches_one_based(&self, value: u32) -> bool {
        match self {
            TimePatternField::Ratio(..) => value >= 1 && self.matches_value(value - 1),
            _ => self.matches_value(value),
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

impl Display for TimePattern {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}-{}-{} {}:{}:{}",
               &self.day_of_week,
               &self.year,
               &self.month,
               &self.day,
               &self.hour,
               &self.minute,
               &self.second
        )
    }
}

impl Display for TimePatternField {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            TimePatternField::Any => write!(f, "*"),
            TimePatternField::Value(v) => write!(f, "{}", v),
            TimePatternField::Range(start, end) => write!(f,"{}..{}", start, end),
            TimePatternField::List(values) => write!(f,"[{}]", values.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(",")),
            TimePatternField::Ratio(divisor, offset) => write!(f,"*/{}", divisor),
        }
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
