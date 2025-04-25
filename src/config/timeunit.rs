use std::time::Duration;
use nom::{branch::alt, bytes::complete::tag, combinator::value};


#[derive(Debug, Clone, Copy)]
pub enum TimeUnit {
    Second,
    Minute,
    Hour,
    Day,
    Week,
    Month,
    Year,
}

impl TimeUnit {
    pub fn parse<'a>(input: &'a str) -> nom::IResult<&'a str, Self> {
        alt((
            value(Self::Second, tag("second")),
            value(Self::Second, tag("s")),
            value(Self::Minute, tag("minute")),
            value(Self::Minute, tag("m")),
            value(Self::Hour, tag("hour")),
            value(Self::Hour, tag("h")),
            value(Self::Day, tag("day")),
            value(Self::Day, tag("d")),
            value(Self::Week, tag("week")),
            value(Self::Week, tag("w")),
            value(Self::Month, tag("month")),
            value(Self::Month, tag("M")),
            value(Self::Year, tag("year")),
            value(Self::Year, tag("y")),
        ))(input)
    }

    pub fn to_duration(&self, amount: u32) -> std::time::Duration {
        match self {
            Self::Second => Duration::from_secs(amount as u64),
            Self::Minute => Duration::from_secs(amount as u64 * 60),
            Self::Hour => Duration::from_secs(amount as u64 * 60 * 60),
            Self::Day => Duration::from_secs(amount as u64 * 60 * 60 * 24),
            Self::Week => Duration::from_secs(amount as u64 * 60 * 60 * 24 * 7),
            Self::Month => Duration::from_secs(amount as u64 * 60 * 60 * 24 * 30),
            Self::Year => Duration::from_secs(amount as u64 * 60 * 60 * 24 * 365),
        }
    }
}
