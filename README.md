# cron-rs

A Rust-based task scheduler that allows you to run commands at specified times using a flexible configuration format.

## Features

- YAML-based configuration
- Flexible scheduling syntax
- Timezone support
- Command execution
- Config validation

## Installation

```bash
cargo install --path .
```

## Usage

1. Create a configuration file (e.g., `config.yml`) with your tasks:

```yaml
tasks:
  - name: MyTask
    cmd: echo 'hello'
    when:
      day_of_week:
        - Mon
        - Thu
      year: '*'
      month: '*'
      day: '*/2'
      hour: 12
      minute: 0
      second: 0
    timezone: 'Europe/Madrid'  # Optional
```

2. Run the scheduler:

```bash
cron-rs --config config.yml
```

3. Validate your configuration:

```bash
cron-rs --config config.yml --validate
```

## Configuration Format

The configuration file supports two formats for specifying when a task should run:

### Detailed Format

```yaml
when:
  day_of_week: [Mon, Tue, Wed, Thu, Fri, Sat, Sun]
  year: '*'  # or specific year
  month: '*' # or specific month
  day: '*'   # or specific day
  hour: '*'  # or specific hour
  minute: '*' # or specific minute
  second: '*' # or specific second
```

### Compact Format

The compact format follows this structure:
`[days_of_week] year-month-day hour:minute:second`

#### Example

```yaml
when: '[Mon,Tue] *-*/2-01..04 12:00:00'
```

- `[Mon,Tue]`: The task will run only on Mondays and Tuesdays
- `*`: Any year
- `*/2`: Every other month (January, March, May, July, September, November)
- `01..04`: Days 1 through 4 of the month
- `12:00:00`: At exactly 12:00:00 (noon)


#### Pattern Syntax

- `*`: Matches any value (wildcard)
- `n`: Exact match (e.g., `5` for the 5th day)
- `n..m`: Range (e.g., `1..5` for days 1 through 5, both included)
- `*/n`: Every nth value (e.g., `*/2` for every other value)
- `[a,b,c]`: List of values (e.g., `[Mon,Wed,Fri]` for those specific days)

You can combine these patterns for powerful scheduling flexibility.


## Timezone Support

You can specify a timezone for each task using the `timezone` field:

```yaml
timezone: 'Europe/Madrid'
```

If not defined, it will use the system's default

## Dependencies

- anyhow
- chrono
- chrono-tz
- clap
- nom
- serde
- serde_yml
- signal-hook
- sysinfo
- iana-time-zone
