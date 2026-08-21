# Dates, Times, and Durations

witchy splits time into three concerns, and keeps them honest with the type
system and the capability model:

- **`duration`** is a span - three hours, thirty seconds. It's data-only arithmetic
  and needs no authority.
- **`time`** turns an instant (milliseconds since the Unix epoch) into a
  civil `DateTime` you can read fields off, and back. Constructing and
  inspecting a `DateTime` is also data-only.
- **Reading the *current* time** is an *effect*, and so it requires the `Clock`
  capability. You can't call "now" without being handed a clock.

That last point is the important one: computing with dates is deterministic and
testable, while *observing* the present is an explicit, granted power.

## Durations

`duration` builds spans from named units and does exact integer arithmetic on
them. You can add durations, compare them, and break one into parts.

```witchy
import duration

fn main(console: Console):
    let d = duration.hours(3) + duration.minutes(45)
    console.print("total minutes: ${duration.to_minutes(d)}")
    console.print("as h:m -> ${duration.part_hours(d)}h ${duration.part_minutes(d)}m")
    let timeout = duration.seconds(30)
    console.print("timeout ms: ${duration.to_milliseconds(timeout)}")
    console.print("longer: ${duration.to_minutes(duration.max(d, timeout))} min")
```

```text
total minutes: 225
as h:m -> 3h 45m
timeout ms: 30000
longer: 225 min
```

`to_minutes` gives the whole span in minutes; `part_minutes` gives just the
minutes *component* after the hours are taken out. Duration literals like `30s`
and `3h` exist as syntax too - `duration.seconds(30)` and `30s` are the same
value.

## Civil dates from an instant

`time.civil(y, mo, da, h, mi, s)` validates a calendar date and returns a
`DateTime` (or a `TimeError` for something like February 30th). From a
`DateTime` you can read every field and a few derived facts:

```witchy
import time

fn main(console: Console):
    match time.civil(2026, 8, 6, 14, 30, 0):
        Ok(d) ->
            console.print("date: ${time.date_string(d)}")
            console.print("weekday: ${time.weekday_name(d)}")
            console.print("month: ${time.month_name(d)}")
            console.print("unix: ${time.to_unix(d)}")
        Err(e) -> console.print("bad date: ${e}")
    console.print("2024 leap? ${time.is_leap(2024)}")
    console.print("2026 leap? ${time.is_leap(2026)}")
```

```text
date: 2026-08-06
weekday: Thursday
month: August
unix: 1786026600
2026 leap? false
```

(`is_leap(2024)` is `true`, printed just above the last line.) `parse_iso8601`
goes the other way, from a string like `"2026-08-06T14:30:00Z"` to a `DateTime`.

## Reading the current time needs a `Clock`

To work with *now*, take a `Clock` parameter and call `clock.now()`, which
returns the current epoch time in milliseconds. Feed that to `time.from_millis`
to get a civil date, and combine with `duration` for deadlines:

```witchy
import time
import duration

fn main(console: Console, clock: Clock):
    let now = time.from_millis(clock.now())
    console.print("today is ${time.weekday_name(now)}, ${time.date_string(now)}")
    let deadline = time.from_millis(clock.now() + duration.to_milliseconds(duration.days(7)))
    console.print("one week out: ${time.date_string(deadline)}")
```

This program's output depends on when you run it, so the book shows it as a
plain snippet rather than a pinned example - but it type-checks, and
`witchy caps` reports its root footprint as exactly `Clock, Console`. That is the
capability model doing its job: direct clock access requires a `Clock` value, and
the root grant is explicit. An ordinary callback can instead delegate a narrower
operation that reads time, so missing `Clock` parameters alone are not a purity
contract. Keep your date math in data-only `time`/`duration` code, take the
`Clock` only at the edge, and the bulk of your logic stays deterministic and
easy to test.
