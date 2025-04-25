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
- Custom working directory
- Configurable output redirection
- Time limits for tasks
- Environment variable support
- Run as different user

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
    working_directory: /path/to/work # Optional, working directory for the task
    stdout: /path/to/stdout.log # Optional
    stderr: /path/to/stderr.log # Optional
    time_limit: 300           # Optional, time limit in seconds
    env:                      # Optional, environment variables
      PATH: /usr/local/bin:/usr/bin:/bin
      HOME: /home/user
    run_as: www-data         # Optional, run as different user
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
- `working_directory`: Working directory for the task (optional, defaults to current directory)
- `stdout`: Path for stdout redirection (optional)
- `stderr`: Path for stderr redirection (optional)
- `time_limit`: Maximum execution time in seconds (optional)
- `env`: Environment variables for the task (optional)
- `run_as`: User to run the task as (optional)

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

### Time Limits

You can set a maximum execution time for tasks. If a task exceeds its time limit, it will be terminated:

```yaml
tasks:
  - name: TimeLimitedTask
    cmd: sleep 600
    every: "1 minute"
    time_limit: 300  # Task will be terminated after 5 minutes
```

### Environment Variables

You can specify environment variables for each task:

```yaml
tasks:
  - name: EnvTask
    cmd: echo $MY_VAR
    every: "5 minutes"
    env:
      MY_VAR: "Hello, World!"
      PATH: /custom/path:/usr/bin:/bin
```

### Running as Different User

You can run tasks as a different user:

```yaml
tasks:
  - name: WebTask
    cmd: touch /var/www/html/test.txt
    every: "1 hour"
    run_as: www-data
```

Note: The scheduler must have sufficient permissions to run commands as the specified user.

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
    working_directory: /path/to/directory
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