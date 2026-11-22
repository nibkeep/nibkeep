# Time

- **Version:** 1

NibKeep distinguishes two kinds of time ([6.3](../Design.md#63-time)): when something was true, the *valid time*, and when it was recorded, the *system time*. This specification describes how both are written and how valid time answers the questions of time travel. The reasons for the choices are in the [technical design](../Technical-design.md#3-time).

## 1. Valid time

### 1.1 Format

Valid time is written in the Extended Date/Time Format ([EDTF](https://www.loc.gov/standards/datetime/), ISO 8601-2), levels 0 and 1:

| Feature | Examples |
| --- | --- |
| Date | `1985-04-12`, `1985-04`, `1985` |
| Date and time | `1985-04-12T23:20:30`, `1985-04-12T23:20:30Z`, `1985-04-12T23:20:30-04`, `1985-04-12T23:20:30+04:30` |
| Interval | `1964/2008`, `2004-06/2006-08`, `2004-02-01/2005` |
| Years with more than four digits | `Y170000002`, `Y-170000002` |
| Negative years | `-1985` |
| Seasons | `2001-21`: 21 spring, 22 summer, 23 autumn, 24 winter |
| Qualifiers | `1984?` uncertain, `2004-06~` approximate, `2004-06-11%` both |
| Unspecified digits | `201X`, `20XX`, `2004-XX`, `1985-04-XX`, `1985-XX-XX` |
| Open and unknown interval ends | `1985/..`, `../1985`, `1985/`, `/1985` |

Further rules:

- A year has exactly four digits, or `Y` and more than four digits, with at most 15 digits in all. `-0000` is not a year.
- Only the last one or two digits of a four-digit year without a sign can be unspecified. A year with unspecified digits stands alone, without month or day. An unspecified month is followed by an unspecified day or by nothing.
- A time runs from `00:00:00` to `23:59:59`; there are no leap seconds. A time zone is `Z` for UTC, `+hh` or `-hh` for whole hours, and `+hh:mm` or `-hh:mm` otherwise, from `-12` to `+14`. `+00`, `-00`, `+00:00` and `+04:00` are rejected, because `Z` and `+04` write the same offsets.
- An interval's ends are dates without times, possibly qualified. Both ends cannot be open or unknown. The start cannot be later than the end: an interval is rejected when its start date begins after its end date ends, compared as written, before qualifiers widen them. So `1985~/1984` is rejected.
- Every value has exactly one text form: the text is printed back exactly as it was accepted. Software that imports dates from elsewhere converts them to these forms.

In CBOR, valid time is a text string.

### 1.2 Possible range

Every date covers a range of days, from the first to the last day it could mean. Days are counted in the proleptic Gregorian calendar.

| Date | Range |
| --- | --- |
| `1985` | 1985-01-01 to 1985-12-31 |
| `198X` | 1980-01-01 to 1989-12-31 |
| `2004-XX` | 2004-01-01 to 2004-12-31 |
| `1985-04` | 1985-04-01 to 1985-04-30 |
| `1985-04-XX` | 1985-04-01 to 1985-04-30 |
| `1985-04-12` | That day |
| `1985-21` spring | 1985-03-01 to 1985-05-31 |
| `1985-22` summer | 1985-06-01 to 1985-08-31 |
| `1985-23` autumn | 1985-09-01 to 1985-11-30 |
| `1985-24` winter | 1985-12-01 to the last day of February 1986 |
| `1985-04-12T23:20:30+04:30` | 1985-04-12, the date as written |

Seasons follow the meteorological seasons of the northern hemisphere.

A qualified date (`?`, `~` or `%`) widens its range by one unit of its precision on each side: `1985~` covers 1984 to 1986, `198X?` covers 1970 to 1999, `1985-04~` covers March to May 1985, a season widens by three months, and a day by one day. Unspecified parts add no precision: `1985-04-XX~` widens like `1985-04~`, and `2004-XX~` like `2004~`. A qualified date is never certain.

## 2. Time travel

Time travel asks two questions about a day ([time travel](../Design.md#63-time)). Each answer is *yes*, *uncertain*, *no*, or *date unknown* when there is no valid time at all, in which case the fact is shown with a "date unknown" marker.

**Does a state hold on that day?** For a single date, the state holds throughout the date's range. The answer is *yes* within the range of an unqualified date without unspecified digits, *uncertain* within the range of any other date, and *no* outside it.

For an interval, the start lies somewhere within the range of the start date, and the end within the range of the end date. The answer is *yes* between the last possible start and the first possible end, *uncertain* elsewhere between the first possible start and the last possible end, and *no* outside. An open end (`..`) means the state held from the beginning of time, or still holds. An unknown end (an empty end) means nobody knows when the state started or ended: the answer is *uncertain* on every day that the other end does not rule out, and never *yes*.

The [data model](model.md#6-current-values) says which question applies to a value: an interval is a state, and any other date an event. In a field with one value, a value also stops where the next one starts.

**Has an event happened by the end of that day?** The answer is *yes* from the last day of the event's range onwards, *uncertain* within the range before that, and *no* before the range. For an interval, the event has happened once it has started.

## 3. System time

System time is a hybrid logical clock timestamp: a pair of integers, the milliseconds since 1970-01-01T00:00:00Z on the device's clock, and a counter. Timestamps are ordered by milliseconds, then by counter. In CBOR, a timestamp is an array of the two integers.

A device's clock follows these rules:

1. **A new local event** gets the device's wall clock time with counter 0, if that is later than the last timestamp. Otherwise it gets the last timestamp's milliseconds and the next counter value. When the counter is used up, the milliseconds advance by one. Timestamps therefore never go backwards, even when the wall clock does.
2. **A timestamp received from another device** becomes the last timestamp if it is later, so every later local event comes after it.
3. **A received timestamp too far ahead** of the device's wall clock, by more than an allowed drift, is rejected and does not change the clock. What happens to the data that carried it is up to the caller.
4. After a restart, the clock resumes from the latest timestamp it handed out or received. Data that has been checked but not accepted yet, such as a change that waits for its parents, does not change the clock; its timestamp counts once the data is accepted.
5. **The last millisecond** is 9999-12-31T23:59:59.999Z. A wall clock that reads later counts as that millisecond. Received timestamps and timestamps to resume from that are later are rejected. A clock that has handed out the last counter value of the last millisecond hands out no more timestamps.

Because a change's timestamp is later than the timestamps of the changes it builds on, the changes with timestamps up to a given time form a consistent history.
