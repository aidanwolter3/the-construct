`core://Text` `format: {date: Relative|Short, past?}` draws every `YYYY-MM-DD` value the text binds as a date the
viewer reads, from the device's own clock and time zone: `Relative` gives "today", "tomorrow", "yesterday", "in 3
days", "3 days ago", else the short date; `Short` gives "Nov 5" (with the year when not this year). With `past`, a
text binding a date before today shows `past` instead ("Overdue").
