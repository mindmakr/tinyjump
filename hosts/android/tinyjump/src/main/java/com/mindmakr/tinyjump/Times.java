package com.mindmakr.tinyjump;

import android.app.DatePickerDialog;
import android.app.TimePickerDialog;
import android.content.Context;
import android.widget.DatePicker;
import android.widget.TimePicker;
import java.text.DateFormat;
import java.text.ParsePosition;
import java.text.SimpleDateFormat;
import java.util.Calendar;
import java.util.Date;
import java.util.Locale;
import java.util.TimeZone;
import org.json.JSONObject;

/**
 * Times for `<Time>` and `<Input type="date|time|datetime|number">`, done by the platform:
 * java.text formats them in the viewer's language, Calendar and TimeZone convert them, and
 * the system pickers choose them. Values are milliseconds (UTC); a time of day is
 * milliseconds since midnight (docs/language.md).
 */
final class Times {
    private Times() {}

    interface Picked {
        void value(Object ms);
    }

    static boolean typed(String type) {
        return "number".equals(type) || "date".equals(type) || "time".equals(type) || "datetime".equals(type);
    }

    private static TimeZone zone(String zone) {
        return "UTC".equals(zone) ? TimeZone.getTimeZone("UTC") : TimeZone.getDefault();
    }

    private static SimpleDateFormat iso(String pattern, TimeZone tz) {
        SimpleDateFormat f = new SimpleDateFormat(pattern, Locale.ROOT);
        f.setTimeZone(tz);
        f.setLenient(false); // 2026-02-30 is not a date
        return f;
    }

    /** What a `<Time>` shows. */
    static String show(JSONObject props) {
        Object v = props.opt("value");
        if (!(v instanceof Number)) return "";
        Date at = new Date(((Number) v).longValue());
        String show = props.optString("show", "datetime");
        TimeZone tz = zone(props.optString("zone", ""));
        DateFormat f;
        if ("iso".equals(props.optString("style", ""))) {
            f = iso("date".equals(show) ? "yyyy-MM-dd" : "time".equals(show) ? "HH:mm" : "yyyy-MM-dd HH:mm", tz);
        } else if ("date".equals(show)) {
            f = DateFormat.getDateInstance(DateFormat.MEDIUM);
        } else if ("time".equals(show)) {
            f = DateFormat.getTimeInstance(DateFormat.SHORT);
        } else {
            f = DateFormat.getDateTimeInstance(DateFormat.MEDIUM, DateFormat.SHORT);
        }
        f.setTimeZone(tz);
        return f.format(at);
    }

    /** A typed input's text as its value, or null when it is not one. */
    static Object value(String type, String zone, String text) {
        String v = text.trim().replace('T', ' ');
        if (v.isEmpty()) return JSONObject.NULL;
        if ("number".equals(type)) {
            try {
                double d = Double.parseDouble(v);
                return Double.isNaN(d) || Double.isInfinite(d) ? JSONObject.NULL : d;
            } catch (NumberFormatException e) {
                return JSONObject.NULL;
            }
        }
        String pattern = "date".equals(type) ? "yyyy-MM-dd" : "time".equals(type) ? "HH:mm" : "yyyy-MM-dd HH:mm";
        // A time of day is the same everywhere: milliseconds since midnight.
        TimeZone tz = "time".equals(type) ? TimeZone.getTimeZone("UTC") : zone(zone);
        ParsePosition at = new ParsePosition(0);
        Date d = iso(pattern, tz).parse(v, at);
        return d == null || at.getIndex() != v.length() ? JSONObject.NULL : (double) d.getTime();
    }

    /** A typed input's value as the text it shows. */
    static String text(String type, String zone, Object value) {
        if (!(value instanceof Number)) return "";
        if ("number".equals(type)) {
            double d = ((Number) value).doubleValue();
            return d == Math.rint(d) && Math.abs(d) < 1e15 ? String.valueOf((long) d) : String.valueOf(d);
        }
        String pattern = "date".equals(type) ? "yyyy-MM-dd" : "time".equals(type) ? "HH:mm" : "yyyy-MM-dd HH:mm";
        TimeZone tz = "time".equals(type) ? TimeZone.getTimeZone("UTC") : zone(zone);
        return iso(pattern, tz).format(new Date(((Number) value).longValue()));
    }

    /** Opens the system's date and/or time picker, starting from the current value. */
    static void pick(final Context ctx, final String type, String zone, Object current, final Picked picked) {
        final boolean timeOnly = "time".equals(type);
        final Calendar c = Calendar.getInstance(timeOnly ? TimeZone.getTimeZone("UTC") : zone(zone));
        if (current instanceof Number) c.setTimeInMillis(((Number) current).longValue());
        if (timeOnly) c.setTimeInMillis(c.get(Calendar.HOUR_OF_DAY) * 3600000L + c.get(Calendar.MINUTE) * 60000L);
        final Runnable askTime = new Runnable() {
            @Override
            public void run() {
                new TimePickerDialog(ctx, new TimePickerDialog.OnTimeSetListener() {
                    @Override
                    public void onTimeSet(TimePicker view, int hour, int minute) {
                        c.set(Calendar.HOUR_OF_DAY, hour);
                        c.set(Calendar.MINUTE, minute);
                        c.set(Calendar.SECOND, 0);
                        c.set(Calendar.MILLISECOND, 0);
                        picked.value((double) c.getTimeInMillis());
                    }
                }, c.get(Calendar.HOUR_OF_DAY), c.get(Calendar.MINUTE), android.text.format.DateFormat.is24HourFormat(ctx)).show();
            }
        };
        if (timeOnly) {
            askTime.run();
            return;
        }
        new DatePickerDialog(ctx, new DatePickerDialog.OnDateSetListener() {
            @Override
            public void onDateSet(DatePicker view, int year, int month, int day) {
                c.set(year, month, day, "date".equals(type) ? 0 : c.get(Calendar.HOUR_OF_DAY), "date".equals(type) ? 0 : c.get(Calendar.MINUTE), 0);
                c.set(Calendar.MILLISECOND, 0);
                if ("datetime".equals(type)) askTime.run();
                else picked.value((double) c.getTimeInMillis());
            }
        }, c.get(Calendar.YEAR), c.get(Calendar.MONTH), c.get(Calendar.DAY_OF_MONTH)).show();
    }
}
