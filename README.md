# cron-rs

A Rust-based task scheduler that allows you to run commands at specified times using a flexible configuration format.

## Features

- YAML-based configuration
- Flexible scheduling syntax
- Timezone support
- Command execution
- Config validation
- Configurable logging (stdout, file, or syslog)
- Concurrent execution prevention
- Configurable output redirection and working directory

## Installation

```bash
cargo install --path .
```

## Usage

1. Create a configuration file (e.g., `config.yml`) with your tasks:

```yaml
logging:
  output: file  # Options: stdout, file, syslog
  level: info   # Options: error, warn, info, debug, trace
  path: /var/log/cron-rs.log  # Required if output is 'file'

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
    avoid_overlapping: true    # Optional, prevents concurrent execution
    runtime_dir: /path/to/work # Optional, working directory for the task
    stdout: /path/to/stdout.log # Optional, defaults to .tmp/stdout.log
    stderr: /path/to/stderr.log # Optional, defaults to .tmp/stderr.log
```

2. Run the scheduler:

```bash
cron-rs --config config.yml
```

3. Validate your configuration:

```bash
cron-rs --config config.yml --validate
```

## Task Configuration Options

### Basic Options
- `name`: Unique identifier for the task
- `cmd`: Command to execute
- `timezone`: Timezone for the task (optional, defaults to system timezone)
- `avoid_overlapping`: Boolean flag to prevent concurrent execution (optional, defaults to false)
- `runtime_dir`: Working directory for the task (optional, defaults to current directory)
- `stdout`: Path for stdout redirection (optional, defaults to .tmp/stdout.log)
- `stderr`: Path for stderr redirection (optional, defaults to .tmp/stderr.log)

### Scheduling Options
You can use either `when` or `every` to specify when a task should run:

#### Using `when`:
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

#### Using `every`:
```yaml
every: "5 minutes"  # or "1 hour", "2 days", etc.
```

## Output Redirection

By default, task output is redirected to files in a `.tmp` directory:
- Standard output goes to `.tmp/stdout.log`
- Standard error goes to `.tmp/stderr.log`

You can customize these paths using the `stdout` and `stderr` options:

```yaml
tasks:
  - name: CustomOutputTask
    cmd: echo "Hello, World!"
    every: "1 minute"
    stdout: /var/log/myapp/stdout.log
    stderr: /var/log/myapp/stderr.log
```

## Working Directory

Tasks run in the current directory by default. You can specify a different working directory using the `runtime_dir` option:

```yaml
tasks:
  - name: WorkInDirectory
    cmd: ls -la
    every: "5 minutes"
    runtime_dir: /path/to/directory
```

## Concurrent Execution Prevention

The `avoid_overlapping` option prevents multiple instances of the same task from running simultaneously. When enabled:

1. The scheduler checks if a previous instance of the task is still running
2. If a previous instance is found, the new execution is skipped
3. A warning is logged when execution is skipped due to overlapping

Example:
```yaml
tasks:
  - name: LongRunningTask
    cmd: sleep 60
    every: "30 seconds"
    avoid_overlapping: true  # This task will never run concurrently
```

## Logging Configuration

The logging configuration supports three output types:

1. `stdout` (default): Logs are written to standard output
2. `file`: Logs are written to a specified file
3. `syslog`: Logs are written to the system syslog

Example configurations:

```yaml
# Log to stdout (default)
logging:
  output: stdout
  level: info

# Log to file
logging:
  output: file
  level: debug
  path: /var/log/cron-rs.log

# Log to syslog
logging:
  output: syslog
  level: warn
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