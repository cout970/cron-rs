use anyhow::{Result, Context, anyhow};
use nom::{branch::alt, bytes::complete::{tag, tag_no_case}, combinator::value};


#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DayOfWeek {
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
}

impl DayOfWeek {
    pub fn parse(input: &str) -> nom::IResult<&str, Self> {
        alt((
            value(Self::Mon, tag_no_case("mon")),
            value(Self::Tue, tag_no_case("tue")),
            value(Self::Wed, tag_no_case("wed")),
            value(Self::Thu, tag_no_case("thu")),
            value(Self::Fri, tag_no_case("fri")),
            value(Self::Sat, tag_no_case("sat")),
            value(Self::Sun, tag_no_case("sun")),
        ))(input)
    }

    pub fn to_u32(self) -> u32 {
        match self {
            Self::Sun => 0,
            Self::Mon => 1,
            Self::Tue => 2,
            Self::Wed => 3,
            Self::Thu => 4,
            Self::Fri => 5,
            Self::Sat => 6,
        }
    }

    pub fn from_u32(n: u32) -> Self {
        match n {
            0 => Self::Sun,
            1 => Self::Mon,
            2 => Self::Tue,
            3 => Self::Wed,
            4 => Self::Thu,
            5 => Self::Fri,
            6 => Self::Sat,
            7 => Self::Sun,
            _ => panic!("Invalid day of week: {}", n),
        }
    }
}

impl TryFrom<u32> for DayOfWeek {
    type Error = anyhow::Error;

    fn try_from(n: u32) -> std::result::Result<Self, Self::Error> {
        match n {
            0 => Ok(Self::Sun),
            1 => Ok(Self::Mon),
            2 => Ok(Self::Tue),
            3 => Ok(Self::Wed),
            4 => Ok(Self::Thu),
            5 => Ok(Self::Fri),
            6 => Ok(Self::Sat),
            7 => Ok(Self::Sun),
            _ => Err(anyhow!("Invalid day of week: {}", n)),
        }
    }
}

impl TryFrom<&str> for DayOfWeek {
    type Error = anyhow::Error;

    fn try_from(input: &str) -> std::result::Result<Self, Self::Error> {
        match input {
            "mon" => Ok(Self::Mon),
            "tue" => Ok(Self::Tue),
            "wed" => Ok(Self::Wed),
            "thu" => Ok(Self::Thu),
            "fri" => Ok(Self::Fri),
            "sat" => Ok(Self::Sat),
            "sun" => Ok(Self::Sun),
            _ => Err(anyhow!("Invalid day of week: {}", input))
        }
    }
}
