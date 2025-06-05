use anyhow::{anyhow, bail, Result};
use nom::{
    branch::alt,
    bytes::complete::tag,
    character::complete::{digit1, space0, space1},
    combinator::{all_consuming, complete, cond, cut, map, map_res, opt, success, value},
    multi::separated_list1,
    sequence::{delimited, pair, preceded, separated_pair, terminated, tuple},
    IResult,
};

use super::{dayofweek::DayOfWeek, number, time_atom, ws, TimePattern, TimePatternField};

// "[Mon,Tue] *-*/2-01..04 12:00:00"

pub fn parse_shorthand(i: &str) -> Result<TimePattern> {
    all_consuming(ws(map_res(
        tuple((
            opt(terminated(dow_part, space0)),
            terminated(cut(date_part), space1),
            cut(hour_part),
        )),
        |(dow_opt, date, hour)| -> anyhow::Result<TimePattern> {
            let dow = dow_opt.unwrap_or(TimePatternField::Any);
            Ok(TimePattern {
                day_of_week: dow,
                year: date[0].clone(),
                month: date[1].clone(),
                day: date[2].clone(),
                hour: hour[0].clone(),
                minute: hour[1].clone(),
                second: hour[2].clone(),
            })
        },
    )))(i)
    .map_err(|e| match e {
        nom::Err::Incomplete(needed) => anyhow!("Unexpected EOF"),
        nom::Err::Error(f) | nom::Err::Failure(f) => {
            let err_pos = i.len() - f.input.len();
            let msg = some_kind_of_uppercase_first_letter(&format!("{f}"));
            anyhow!("{}pattern position {}\n{}\n{}^", msg, err_pos, i, " ".repeat(err_pos))
        },
    })
    .map(|(_, pattern)| pattern)
}

// https://stackoverflow.com/questions/38406793/why-is-capitalizing-the-first-letter-of-a-string-so-convoluted-in-rust
fn some_kind_of_uppercase_first_letter(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
    }
}

fn dow_part(i: &str) -> IResult<&str, TimePatternField> {
    single_field(true)(i)
}

fn date_part(i: &str) -> IResult<&str, [TimePatternField; 3]> {
    map(
        tuple((
            single_field(false),
            tag("-"),
            single_field(false),
            tag("-"),
            single_field(false),
        )),
        |(year, _, month, _, day)| [year, month, day],
    )(i)
}

fn hour_part(i: &str) -> IResult<&str, [TimePatternField; 3]> {
    map(
        tuple((
            single_field(false),
            tag(":"),
            single_field(false),
            tag(":"),
            single_field(false),
        )),
        |(hour, _, minute, _, second)| [hour, minute, second],
    )(i)
}

pub fn single_field<'a>(
    allow_dow: bool,
) -> impl FnMut(&'a str) -> IResult<&'a str, TimePatternField> {
    // Alt between list, range, ratio, value, any
    // Fallback to any
    // Do once
    alt((
        range(allow_dow),
        ratio(),
        list(allow_dow),
        simple(allow_dow),
        any(),
    ))
}

pub fn any<'a>() -> impl FnMut(&'a str) -> IResult<&'a str, TimePatternField> {
    value(TimePatternField::Any, tag("*"))
}

pub fn simple<'a>(allow_dow: bool) -> impl FnMut(&'a str) -> IResult<&'a str, TimePatternField> {
    map(time_atom(allow_dow), |value| TimePatternField::Value(value))
}

pub fn list<'a>(allow_dow: bool) -> impl FnMut(&'a str) -> IResult<&'a str, TimePatternField> {
    map(
        delimited(
            tuple((tag("["), space0)),
            cut(separated_list1(ws(tag(",")), ws(time_atom(allow_dow)))),
            tuple((space0, tag("]"))),
        ),
        |elements| TimePatternField::List(elements),
    )
}

pub fn range<'a>(allow_dow: bool) -> impl FnMut(&'a str) -> IResult<&'a str, TimePatternField> {
    map(
        separated_pair(
            time_atom(allow_dow),
            ws(alt((tag(".."), tag("..=")))),
            cut(time_atom(allow_dow)),
        ),
        |(a, b)| TimePatternField::Range(a, b),
    )
}

pub fn ratio<'a>() -> impl FnMut(&'a str) -> IResult<&'a str, TimePatternField> {
    map(
        tuple((
            any(),
            ws(tag("/")),
            cut(number),
            opt(preceded(ws(tag("+")), number)),
        )),
        |(_, _, ratio, offset)| {
            let offset = offset.unwrap_or(0);
            TimePatternField::Ratio(ratio, offset)
        },
    )
}
