/** The choices offered for an event's reminders, in minutes before its start. */
export const REMINDER_CHOICES = [0, 5, 10, 15, 30, 60, 120, 1440, 2880, 10080]

/** The i18n key and count of a reminder: "10 min before", "1 h before", "1 day before". */
export function reminderKey(m: number): [string, number] {
  if (m === 0) return ['tools.calendar.reminder.atStart', 0]
  if (m % 1440 === 0) return ['tools.calendar.reminder.days', m / 1440]
  if (m % 60 === 0) return ['tools.calendar.reminder.hours', m / 60]
  return ['tools.calendar.reminder.minutes', m]
}
