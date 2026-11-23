package com.mindmakr.tinyjump.plugins;

import android.Manifest;
import android.app.Activity;
import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.content.ClipData;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.database.Cursor;
import android.graphics.Bitmap;
import android.location.Location;
import android.location.LocationListener;
import android.location.LocationManager;
import android.net.Uri;
import android.os.Build;
import android.os.Bundle;
import android.os.CancellationSignal;
import android.os.Handler;
import android.os.Looper;
import android.provider.MediaStore;
import android.provider.OpenableColumns;
import android.util.Base64;
import com.mindmakr.tinyjump.DevicePlugin;
import com.mindmakr.tinyjump.TinyJump;
import java.io.ByteArrayOutputStream;
import java.io.InputStream;
import java.io.OutputStream;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.HashSet;
import java.util.List;
import java.util.Set;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.atomic.AtomicInteger;
import org.json.JSONArray;
import org.json.JSONObject;

/**
 * Opt-in Android device capability plugins (plain Java 11, minSdk 21, zero AndroidX/Kotlin).
 * Copied into generated Android apps by `tj build --android` only when the app declares
 * matching device permissions (`files`, `notifications`, `location`, `camera`).
 */
public final class DevicePlugins {
    private static final int DEFAULT_MAX_FILE_BYTES = 10 * 1024 * 1024;
    private static final int HARD_MAX_FILE_BYTES = 10 * 1024 * 1024;
    private static final int MAX_BASE64_CHARS = ((HARD_MAX_FILE_BYTES + 2) / 3) * 4 + 4;
    private static final long LAST_KNOWN_MAX_AGE_MS = 5 * 60 * 1000L;
    private static final String NOTIF_CHANNEL_ID = "tinyjump_default";
    private static final String NOTIF_CHANNEL_NAME = "Notifications";

    private static final int REQ_FILE_PICK = 0x7101;
    private static final int REQ_FILE_SAVE = 0x7102;
    private static final int REQ_CAMERA = 0x7103;
    private static final int REQ_PERM_NOTIF = 0x7201;
    private static final int REQ_PERM_LOCATION = 0x7202;
    private static final int REQ_PERM_CAMERA = 0x7203;

    private static final Handler MAIN = new Handler(Looper.getMainLooper());
    private static final ExecutorService IO = Executors.newCachedThreadPool();
    private static final AtomicInteger NEXT_NOTIF_ID = new AtomicInteger(1);

    private static final Object LOCK = new Object();
    private static Activity activeActivity;
    private static final Set<String> REGISTERED_CAPS = new HashSet<>();

    private static PendingPick pendingPick;
    private static PendingSave pendingSave;
    private static PendingCamera pendingCamera;
    private static TinyJump.Reply pendingNotifPerm;
    private static PendingLocation pendingLocation;
    private static boolean locationInFlight;
    private static Runnable cancelActiveLocation;

    private static final class PendingPick {
        final TinyJump.Reply reply;
        final String readAs;
        final int maxBytes;

        PendingPick(TinyJump.Reply reply, String readAs, int maxBytes) {
            this.reply = reply;
            this.readAs = readAs;
            this.maxBytes = maxBytes;
        }
    }

    private static final class PendingSave {
        final TinyJump.Reply reply;
        final String name;
        final byte[] data;

        PendingSave(TinyJump.Reply reply, String name, byte[] data) {
            this.reply = reply;
            this.name = name;
            this.data = data;
        }
    }

    private static final class PendingCamera {
        final TinyJump.Reply reply;
        final String facing;
        final int maxDimension;
        final int quality;

        PendingCamera(TinyJump.Reply reply, String facing, int maxDimension, int quality) {
            this.reply = reply;
            this.facing = facing;
            this.maxDimension = maxDimension;
            this.quality = quality;
        }
    }

    private static final class PendingLocation {
        final TinyJump.Reply reply;
        final boolean highAccuracy;
        final int timeoutMs;

        PendingLocation(TinyJump.Reply reply, boolean highAccuracy, int timeoutMs) {
            this.reply = reply;
            this.highAccuracy = highAccuracy;
            this.timeoutMs = timeoutMs;
        }
    }

    private DevicePlugins() {}

    public static void install(Activity activity, Set<String> capabilities) {
        drainPending("activity replaced");
        Set<String> prevCaps;
        synchronized (LOCK) {
            activeActivity = activity;
            prevCaps = new HashSet<>(REGISTERED_CAPS);
            REGISTERED_CAPS.clear();
        }
        for (String cap : prevCaps) {
            TinyJump.registerPlugin(cap, null);
        }
        if (capabilities.contains("files")) {
            synchronized (LOCK) { REGISTERED_CAPS.add("files"); }
            TinyJump.registerPlugin("files", new DevicePlugin() {
                @Override
                public void call(Context ctx, String method, JSONObject args, TinyJump.Reply reply) throws Exception {
                    handleFiles(method, args, reply);
                }
            });
        }
        if (capabilities.contains("notifications")) {
            synchronized (LOCK) { REGISTERED_CAPS.add("notifications"); }
            TinyJump.registerPlugin("notifications", new DevicePlugin() {
                @Override
                public void call(Context ctx, String method, JSONObject args, TinyJump.Reply reply) throws Exception {
                    handleNotifications(ctx, method, args, reply);
                }
            });
        }
        if (capabilities.contains("location") || capabilities.contains("location-background")) {
            synchronized (LOCK) { REGISTERED_CAPS.add("location"); }
            TinyJump.registerPlugin("location", new DevicePlugin() {
                @Override
                public void call(Context ctx, String method, JSONObject args, TinyJump.Reply reply) throws Exception {
                    handleLocation(method, args, reply);
                }
            });
        }
        if (capabilities.contains("camera")) {
            synchronized (LOCK) { REGISTERED_CAPS.add("camera"); }
            TinyJump.registerPlugin("camera", new DevicePlugin() {
                @Override
                public void call(Context ctx, String method, JSONObject args, TinyJump.Reply reply) throws Exception {
                    handleCamera(method, args, reply);
                }
            });
        }
    }

    public static void uninstall(Activity activity) {
        Set<String> toClear;
        synchronized (LOCK) {
            if (activeActivity != activity) return;
            activeActivity = null;
            toClear = new HashSet<>(REGISTERED_CAPS);
            REGISTERED_CAPS.clear();
        }
        for (String cap : toClear) {
            TinyJump.registerPlugin(cap, null);
        }
        drainPending("activity destroyed");
    }

    private static void drainPending(String reason) {
        PendingPick pick;
        PendingSave save;
        PendingCamera cam;
        TinyJump.Reply notif;
        PendingLocation loc;
        Runnable cancelLoc;
        synchronized (LOCK) {
            pick = pendingPick; pendingPick = null;
            save = pendingSave; pendingSave = null;
            cam = pendingCamera; pendingCamera = null;
            notif = pendingNotifPerm; pendingNotifPerm = null;
            loc = pendingLocation; pendingLocation = null;
            locationInFlight = false;
            cancelLoc = cancelActiveLocation; cancelActiveLocation = null;
        }
        if (cancelLoc != null) MAIN.post(cancelLoc);
        if (pick != null) pick.reply.done(null, reason);
        if (save != null) save.reply.done(null, reason);
        if (cam != null) cam.reply.done(null, reason);
        if (notif != null) notif.done(null, reason);
        if (loc != null) loc.reply.done(null, reason);
    }

    private static Activity requireActivity() {
        synchronized (LOCK) {
            if (activeActivity == null || activeActivity.isFinishing()) {
                throw new IllegalStateException("no active Android activity");
            }
            return activeActivity;
        }
    }

    private static boolean isStillActive(Activity act) {
        synchronized (LOCK) {
            return activeActivity == act && !act.isFinishing();
        }
    }

    private static String optStringExact(JSONObject args, String key) {
        if (args == null || args.isNull(key)) return null;
        Object v = args.opt(key);
        return v instanceof String ? (String) v : null;
    }

    private static int clampInt(JSONObject args, String key, int fallback, int min, int max) {
        if (args == null || args.isNull(key)) return fallback;
        Object raw = args.opt(key);
        if (!(raw instanceof Number)) return fallback;
        double v = ((Number) raw).doubleValue();
        if (Double.isNaN(v) || Double.isInfinite(v)) return fallback;
        if (v < min) return min;
        if (v > max) return max;
        return (int) Math.floor(v);
    }

    private static double clampDouble(JSONObject args, String key, double fallback, double min, double max) {
        if (args == null || args.isNull(key)) return fallback;
        Object raw = args.opt(key);
        if (!(raw instanceof Number)) return fallback;
        double v = ((Number) raw).doubleValue();
        if (Double.isNaN(v) || Double.isInfinite(v)) return fallback;
        if (v < min) return min;
        if (v > max) return max;
        return v;
    }

    private static String sanitizeFilename(String raw) {
        if (raw == null) throw new IllegalArgumentException("files.save requires a string \"name\"");
        String trimmed = raw.trim();
        if (trimmed.isEmpty() || ".".equals(trimmed) || "..".equals(trimmed)) {
            throw new IllegalArgumentException("files.save requires a valid filename");
        }
        String leaf = trimmed
            .replaceAll("[\\\\/]+", "_")
            .replaceAll("\\.\\.+", "_")
            .replaceAll("[\\x00-\\x1f\\x7f]", "")
            .trim();
        if (leaf.isEmpty() || ".".equals(leaf) || "..".equals(leaf) || leaf.matches("^_+$")) {
            throw new IllegalArgumentException("files.save requires a valid filename");
        }
        return leaf.length() > 180 ? leaf.substring(0, 180) : leaf;
    }

    private static String bareMime(String raw, String fallback) {
        if (raw == null) return fallback;
        int semi = raw.indexOf(';');
        String s = (semi >= 0 ? raw.substring(0, semi) : raw).trim();
        return s.isEmpty() ? fallback : s;
    }

    // ---- files ----

    private static void handleFiles(String method, JSONObject args, final TinyJump.Reply reply) throws Exception {
        if ("pick".equals(method)) {
            String rawReadAs = optStringExact(args, "readAs");
            if (args != null && !args.isNull("readAs") && rawReadAs == null) {
                reply.done(null, "files.pick \"readAs\" must be \"text\" or \"base64\"");
                return;
            }
            final String readAs = rawReadAs == null ? "text" : rawReadAs;
            if (!"text".equals(readAs) && !"base64".equals(readAs)) {
                reply.done(null, "files.pick \"readAs\" must be \"text\" or \"base64\"");
                return;
            }
            final int maxBytes = clampInt(args, "maxBytes", DEFAULT_MAX_FILE_BYTES, 1, HARD_MAX_FILE_BYTES);
            final boolean multiple = args != null && Boolean.TRUE.equals(args.opt("multiple"));
            final String accept = optStringExact(args, "accept");
            final List<String> mimes = new ArrayList<>();
            if (accept != null) {
                for (String part : accept.split(",")) {
                    String t = bareMime(part, "");
                    if (t.contains("/") && !t.startsWith(".")) mimes.add(t);
                }
            }
            final Activity act = requireActivity();
            final PendingPick mine = new PendingPick(reply, readAs, maxBytes);
            synchronized (LOCK) {
                if (pendingPick != null) {
                    reply.done(null, "a file picker is already open");
                    return;
                }
                pendingPick = mine;
            }
            MAIN.post(new Runnable() {
                @Override
                public void run() {
                    synchronized (LOCK) {
                        if (pendingPick != mine || !isStillActive(act)) {
                            if (pendingPick == mine) pendingPick = null;
                            reply.done(null, "activity destroyed");
                            return;
                        }
                    }
                    try {
                        Intent intent = new Intent(Intent.ACTION_OPEN_DOCUMENT);
                        intent.addCategory(Intent.CATEGORY_OPENABLE);
                        if (mimes.size() == 1) {
                            intent.setType(mimes.get(0));
                        } else if (mimes.size() > 1) {
                            intent.setType("*/*");
                            intent.putExtra(Intent.EXTRA_MIME_TYPES, mimes.toArray(new String[0]));
                        } else {
                            intent.setType("*/*");
                        }
                        if (multiple) intent.putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true);
                        act.startActivityForResult(intent, REQ_FILE_PICK);
                    } catch (Throwable t) {
                        synchronized (LOCK) {
                            if (pendingPick == mine) pendingPick = null;
                        }
                        reply.done(null, t.getMessage() != null ? t.getMessage() : "failed to open file picker");
                    }
                }
            });
            return;
        }

        if ("save".equals(method)) {
            final String name = sanitizeFilename(optStringExact(args, "name"));
            final String textVal = optStringExact(args, "text");
            final String b64Val = optStringExact(args, "base64");
            if (textVal == null && b64Val == null) {
                reply.done(null, "files.save requires either \"text\" or \"base64\"");
                return;
            }
            if (textVal != null && b64Val != null) {
                reply.done(null, "files.save accepts either \"text\" or \"base64\", not both");
                return;
            }
            final byte[] data;
            final String mime;
            if (b64Val != null) {
                String compact = b64Val.replaceAll("\\s+", "");
                if (compact.length() > MAX_BASE64_CHARS) {
                    reply.done(null, "file exceeds maximum size limit");
                    return;
                }
                try {
                    data = Base64.decode(compact, Base64.DEFAULT);
                } catch (IllegalArgumentException e) {
                    reply.done(null, "invalid base64 payload");
                    return;
                }
                mime = bareMime(optStringExact(args, "mime"), "application/octet-stream");
            } else {
                data = textVal.getBytes(StandardCharsets.UTF_8);
                mime = bareMime(optStringExact(args, "mime"), "text/plain");
            }
            if (data.length > HARD_MAX_FILE_BYTES) {
                reply.done(null, "file exceeds maximum size limit");
                return;
            }
            final Activity act = requireActivity();
            final PendingSave mine = new PendingSave(reply, name, data);
            synchronized (LOCK) {
                if (pendingSave != null) {
                    reply.done(null, "a file save dialog is already open");
                    return;
                }
                pendingSave = mine;
            }
            MAIN.post(new Runnable() {
                @Override
                public void run() {
                    synchronized (LOCK) {
                        if (pendingSave != mine || !isStillActive(act)) {
                            if (pendingSave == mine) pendingSave = null;
                            reply.done(null, "activity destroyed");
                            return;
                        }
                    }
                    try {
                        Intent intent = new Intent(Intent.ACTION_CREATE_DOCUMENT);
                        intent.addCategory(Intent.CATEGORY_OPENABLE);
                        intent.setType(mime);
                        intent.putExtra(Intent.EXTRA_TITLE, name);
                        act.startActivityForResult(intent, REQ_FILE_SAVE);
                    } catch (Throwable t) {
                        synchronized (LOCK) {
                            if (pendingSave == mine) pendingSave = null;
                        }
                        reply.done(null, t.getMessage() != null ? t.getMessage() : "failed to open save dialog");
                    }
                }
            });
            return;
        }

        reply.done(null, "unknown files method: " + method);
    }

    private static byte[] readUriBounded(Context ctx, Uri uri, int maxBytes) throws Exception {
        try (InputStream in = ctx.getContentResolver().openInputStream(uri)) {
            if (in == null) throw new IllegalStateException("cannot open selected file");
            ByteArrayOutputStream out = new ByteArrayOutputStream();
            byte[] buf = new byte[8192];
            int total = 0;
            int n;
            while ((n = in.read(buf)) > 0) {
                total += n;
                if (total > maxBytes) {
                    throw new IllegalArgumentException("file exceeds maximum size limit");
                }
                out.write(buf, 0, n);
            }
            return out.toByteArray();
        }
    }

    private static String queryDisplayName(Context ctx, Uri uri) {
        try (Cursor c = ctx.getContentResolver().query(uri, new String[]{OpenableColumns.DISPLAY_NAME}, null, null, null)) {
            if (c != null && c.moveToFirst()) {
                int idx = c.getColumnIndex(OpenableColumns.DISPLAY_NAME);
                if (idx >= 0) {
                    String s = c.getString(idx);
                    if (s != null && !s.isEmpty()) return s;
                }
            }
        } catch (Throwable ignored) {}
        String last = uri.getLastPathSegment();
        return last != null && !last.isEmpty() ? last : "file";
    }

    // ---- notifications ----

    private static boolean hasNotifPermission(Context ctx) {
        if (Build.VERSION.SDK_INT >= 33) {
            return ctx.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED;
        }
        NotificationManager nm = (NotificationManager) ctx.getSystemService(Context.NOTIFICATION_SERVICE);
        if (nm != null && Build.VERSION.SDK_INT >= 24) {
            return nm.areNotificationsEnabled();
        }
        return true;
    }

    private static void handleNotifications(Context ctx, String method, JSONObject args, final TinyJump.Reply reply) throws Exception {
        if ("requestPermission".equals(method)) {
            if (Build.VERSION.SDK_INT < 33) {
                boolean granted = hasNotifPermission(ctx);
                JSONObject res = new JSONObject();
                res.put("granted", granted);
                res.put("status", granted ? "granted" : "denied");
                reply.done(res, null);
                return;
            }
            if (hasNotifPermission(ctx)) {
                JSONObject res = new JSONObject();
                res.put("granted", true);
                res.put("status", "granted");
                reply.done(res, null);
                return;
            }
            final Activity act = requireActivity();
            synchronized (LOCK) {
                if (pendingNotifPerm != null) {
                    reply.done(null, "a notification permission request is already in progress");
                    return;
                }
                pendingNotifPerm = reply;
            }
            MAIN.post(new Runnable() {
                @Override
                public void run() {
                    synchronized (LOCK) {
                        if (pendingNotifPerm != reply || !isStillActive(act)) {
                            if (pendingNotifPerm == reply) pendingNotifPerm = null;
                            reply.done(null, "activity destroyed");
                            return;
                        }
                    }
                    try {
                        if (Build.VERSION.SDK_INT >= 33) {
                            act.requestPermissions(new String[]{Manifest.permission.POST_NOTIFICATIONS}, REQ_PERM_NOTIF);
                        }
                    } catch (Throwable t) {
                        synchronized (LOCK) {
                            if (pendingNotifPerm == reply) pendingNotifPerm = null;
                        }
                        reply.done(null, t.getMessage() != null ? t.getMessage() : "permission request failed");
                    }
                }
            });
            return;
        }

        if ("notify".equals(method)) {
            if (!hasNotifPermission(ctx)) {
                reply.done(null, "notification permission not granted");
                return;
            }
            String rawTitle = optStringExact(args, "title");
            String title = rawTitle == null ? "" : rawTitle.trim();
            if (title.isEmpty()) {
                reply.done(null, "notifications.notify requires a non-empty \"title\"");
                return;
            }
            if (title.length() > 256) {
                reply.done(null, "notifications.notify \"title\" is too long");
                return;
            }
            String rawBody = optStringExact(args, "body");
            String body = rawBody == null ? "" : (rawBody.length() > 2048 ? rawBody.substring(0, 2048) : rawBody);
            String rawId = optStringExact(args, "id");
            String id = rawId == null ? "" : rawId.trim();
            if (id.isEmpty()) {
                id = "tj-" + NEXT_NOTIF_ID.getAndIncrement();
            } else if (id.length() > 128) {
                id = id.substring(0, 128);
            }
            NotificationManager nm = (NotificationManager) ctx.getSystemService(Context.NOTIFICATION_SERVICE);
            if (nm == null) {
                reply.done(null, "notification service unavailable");
                return;
            }
            Notification.Builder builder;
            if (Build.VERSION.SDK_INT >= 26) {
                NotificationChannel ch = new NotificationChannel(NOTIF_CHANNEL_ID, NOTIF_CHANNEL_NAME, NotificationManager.IMPORTANCE_DEFAULT);
                nm.createNotificationChannel(ch);
                builder = new Notification.Builder(ctx, NOTIF_CHANNEL_ID);
            } else {
                builder = new Notification.Builder(ctx);
            }
            builder.setContentTitle(title)
                .setContentText(body)
                .setSmallIcon(android.R.drawable.ic_dialog_info)
                .setAutoCancel(true);
            nm.notify(id, 0, builder.build());
            JSONObject res = new JSONObject();
            res.put("delivered", true);
            res.put("id", id);
            reply.done(res, null);
            return;
        }

        reply.done(null, "unknown notifications method: " + method);
    }

    // ---- location ----

    private static boolean hasLocationPermission(Context ctx) {
        if (Build.VERSION.SDK_INT < 23) return true;
        return ctx.checkSelfPermission(Manifest.permission.ACCESS_FINE_LOCATION) == PackageManager.PERMISSION_GRANTED
            || ctx.checkSelfPermission(Manifest.permission.ACCESS_COARSE_LOCATION) == PackageManager.PERMISSION_GRANTED;
    }

    private static void handleLocation(String method, JSONObject args, final TinyJump.Reply reply) {
        if (!"currentPosition".equals(method)) {
            reply.done(null, "unknown location method: " + method);
            return;
        }
        final boolean highAccuracy = args != null && Boolean.TRUE.equals(args.opt("highAccuracy"));
        final int timeoutMs = clampInt(args, "timeoutMs", 15000, 1000, 60000);
        final Activity act = requireActivity();
        if (hasLocationPermission(act)) {
            synchronized (LOCK) {
                if (locationInFlight || pendingLocation != null) {
                    reply.done(null, "a location request is already in progress");
                    return;
                }
                locationInFlight = true;
            }
            fetchLocation(act, highAccuracy, timeoutMs, reply);
            return;
        }
        final PendingLocation mine = new PendingLocation(reply, highAccuracy, timeoutMs);
        synchronized (LOCK) {
            if (locationInFlight || pendingLocation != null) {
                reply.done(null, "a location request is already in progress");
                return;
            }
            pendingLocation = mine;
        }
        MAIN.post(new Runnable() {
            @Override
            public void run() {
                synchronized (LOCK) {
                    if (pendingLocation != mine || !isStillActive(act)) {
                        if (pendingLocation == mine) pendingLocation = null;
                        reply.done(null, "activity destroyed");
                        return;
                    }
                }
                try {
                    if (Build.VERSION.SDK_INT >= 23) {
                        act.requestPermissions(
                            new String[]{Manifest.permission.ACCESS_FINE_LOCATION, Manifest.permission.ACCESS_COARSE_LOCATION},
                            REQ_PERM_LOCATION
                        );
                    }
                } catch (Throwable t) {
                    synchronized (LOCK) {
                        if (pendingLocation == mine) pendingLocation = null;
                    }
                    reply.done(null, t.getMessage() != null ? t.getMessage() : "location permission request failed");
                }
            }
        });
    }

    @SuppressWarnings("MissingPermission")
    private static void fetchLocation(final Activity act, final boolean highAccuracy, final int timeoutMs, final TinyJump.Reply reply) {
        final Context appCtx = act.getApplicationContext();
        MAIN.post(new Runnable() {
            @Override
            public void run() {
                if (!isStillActive(act)) {
                    synchronized (LOCK) { locationInFlight = false; }
                    reply.done(null, "activity destroyed");
                    return;
                }
                try {
                    final LocationManager lm = (LocationManager) appCtx.getSystemService(Context.LOCATION_SERVICE);
                    if (lm == null) {
                        synchronized (LOCK) { locationInFlight = false; }
                        reply.done(null, "location service unavailable");
                        return;
                    }
                    String preferred = highAccuracy ? LocationManager.GPS_PROVIDER : LocationManager.NETWORK_PROVIDER;
                    String fallback = highAccuracy ? LocationManager.NETWORK_PROVIDER : LocationManager.GPS_PROVIDER;
                    String provider = pickProvider(lm, preferred, fallback);
                    if (provider == null) {
                        synchronized (LOCK) { locationInFlight = false; }
                        reply.done(null, "no location provider enabled");
                        return;
                    }
                    final String chosen = provider;
                    if (Build.VERSION.SDK_INT >= 30) {
                        final CancellationSignal signal = new CancellationSignal();
                        final Runnable[] cancelRef = new Runnable[1];
                        final Runnable timeout = new Runnable() {
                            @Override
                            public void run() {
                                synchronized (LOCK) {
                                    if (cancelActiveLocation == cancelRef[0]) {
                                        cancelActiveLocation = null;
                                        locationInFlight = false;
                                    }
                                }
                                signal.cancel();
                                deliverLastKnownOrError(lm, chosen, reply, "location request timed out");
                            }
                        };
                        cancelRef[0] = new Runnable() {
                            @Override
                            public void run() {
                                MAIN.removeCallbacks(timeout);
                                signal.cancel();
                                reply.done(null, "activity destroyed");
                            }
                        };
                        synchronized (LOCK) {
                            if (!isStillActive(act)) {
                                locationInFlight = false;
                                reply.done(null, "activity destroyed");
                                return;
                            }
                            cancelActiveLocation = cancelRef[0];
                        }
                        MAIN.postDelayed(timeout, timeoutMs);
                        try {
                            lm.getCurrentLocation(chosen, signal, appCtx.getMainExecutor(), new java.util.function.Consumer<Location>() {
                                @Override
                                public void accept(Location loc) {
                                    synchronized (LOCK) {
                                        if (cancelActiveLocation == cancelRef[0]) {
                                            cancelActiveLocation = null;
                                            locationInFlight = false;
                                        }
                                    }
                                    MAIN.removeCallbacks(timeout);
                                    if (loc != null) deliverLocation(loc, reply);
                                    else deliverLastKnownOrError(lm, chosen, reply, "location unavailable");
                                }
                            });
                        } catch (Throwable t) {
                            synchronized (LOCK) {
                                if (cancelActiveLocation == cancelRef[0]) {
                                    cancelActiveLocation = null;
                                    locationInFlight = false;
                                }
                            }
                            MAIN.removeCallbacks(timeout);
                            reply.done(null, t.getMessage() != null ? t.getMessage() : "location unavailable");
                        }
                    } else {
                        final LocationListener[] holder = new LocationListener[1];
                        final Runnable[] cancelRef = new Runnable[1];
                        final Runnable timeout = new Runnable() {
                            @Override
                            public void run() {
                                synchronized (LOCK) {
                                    if (cancelActiveLocation == cancelRef[0]) {
                                        cancelActiveLocation = null;
                                        locationInFlight = false;
                                    }
                                }
                                if (holder[0] != null) lm.removeUpdates(holder[0]);
                                deliverLastKnownOrError(lm, chosen, reply, "location request timed out");
                            }
                        };
                        LocationListener listener = new LocationListener() {
                            @Override
                            public void onLocationChanged(Location loc) {
                                synchronized (LOCK) {
                                    if (cancelActiveLocation == cancelRef[0]) {
                                        cancelActiveLocation = null;
                                        locationInFlight = false;
                                    }
                                }
                                MAIN.removeCallbacks(timeout);
                                lm.removeUpdates(this);
                                deliverLocation(loc, reply);
                            }
                            @Override public void onStatusChanged(String p, int s, Bundle e) {}
                            @Override public void onProviderEnabled(String p) {}
                            @Override public void onProviderDisabled(String p) {}
                        };
                        holder[0] = listener;
                        cancelRef[0] = new Runnable() {
                            @Override
                            public void run() {
                                MAIN.removeCallbacks(timeout);
                                lm.removeUpdates(listener);
                                reply.done(null, "activity destroyed");
                            }
                        };
                        synchronized (LOCK) {
                            if (!isStillActive(act)) {
                                locationInFlight = false;
                                reply.done(null, "activity destroyed");
                                return;
                            }
                            cancelActiveLocation = cancelRef[0];
                        }
                        MAIN.postDelayed(timeout, timeoutMs);
                        try {
                            lm.requestSingleUpdate(chosen, listener, Looper.getMainLooper());
                        } catch (Throwable t) {
                            synchronized (LOCK) {
                                if (cancelActiveLocation == cancelRef[0]) {
                                    cancelActiveLocation = null;
                                    locationInFlight = false;
                                }
                            }
                            MAIN.removeCallbacks(timeout);
                            reply.done(null, t.getMessage() != null ? t.getMessage() : "location unavailable");
                        }
                    }
                } catch (Throwable t) {
                    synchronized (LOCK) {
                        cancelActiveLocation = null;
                        locationInFlight = false;
                    }
                    reply.done(null, t.getMessage() != null ? t.getMessage() : "location unavailable");
                }
            }
        });
    }

    private static String pickProvider(LocationManager lm, String preferred, String fallback) {
        String[] candidates = new String[]{preferred, fallback, LocationManager.PASSIVE_PROVIDER};
        for (String p : candidates) {
            try {
                if (lm.isProviderEnabled(p)) return p;
            } catch (Throwable ignored) {}
        }
        return null;
    }

    @SuppressWarnings("MissingPermission")
    private static void deliverLastKnownOrError(LocationManager lm, String provider, TinyJump.Reply reply, String err) {
        try {
            Location last = lm.getLastKnownLocation(provider);
            if (last != null && (System.currentTimeMillis() - last.getTime()) <= LAST_KNOWN_MAX_AGE_MS) {
                deliverLocation(last, reply);
                return;
            }
        } catch (Throwable ignored) {}
        reply.done(null, err);
    }

    private static void deliverLocation(Location loc, TinyJump.Reply reply) {
        try {
            double lat = loc.getLatitude();
            double lng = loc.getLongitude();
            if (Double.isNaN(lat) || Double.isInfinite(lat) || Double.isNaN(lng) || Double.isInfinite(lng)) {
                reply.done(null, "location returned non-finite coordinates");
                return;
            }
            float acc = loc.getAccuracy();
            double accuracy = (loc.hasAccuracy() && !Float.isNaN(acc) && !Float.isInfinite(acc) && acc >= 0f) ? acc : 0.0;
            JSONObject res = new JSONObject();
            res.put("lat", lat);
            res.put("lng", lng);
            res.put("accuracy", accuracy);
            if (loc.hasAltitude() && !Double.isNaN(loc.getAltitude()) && !Double.isInfinite(loc.getAltitude())) {
                res.put("altitude", loc.getAltitude());
            } else {
                res.put("altitude", JSONObject.NULL);
            }
            res.put("timestamp", loc.getTime() > 0 ? loc.getTime() : System.currentTimeMillis());
            reply.done(res, null);
        } catch (Throwable t) {
            reply.done(null, t.getMessage() != null ? t.getMessage() : "location serialization failed");
        }
    }

    // ---- camera ----

    private static void handleCamera(String method, JSONObject args, final TinyJump.Reply reply) {
        if (!"capture".equals(method)) {
            reply.done(null, "unknown camera method: " + method);
            return;
        }
        String rawFacing = optStringExact(args, "facing");
        if (args != null && !args.isNull("facing") && rawFacing == null) {
            reply.done(null, "camera.capture \"facing\" must be \"back\" or \"front\"");
            return;
        }
        final String facing = rawFacing == null ? "back" : rawFacing;
        if (!"back".equals(facing) && !"front".equals(facing)) {
            reply.done(null, "camera.capture \"facing\" must be \"back\" or \"front\"");
            return;
        }
        final int maxDim = clampInt(args, "maxDimension", 1280, 64, 4096);
        final int quality = (int) Math.round(clampDouble(args, "quality", 0.85, 0.1, 1.0) * 100.0);
        final Activity act = requireActivity();
        final PendingCamera mine = new PendingCamera(reply, facing, maxDim, quality);
        synchronized (LOCK) {
            if (pendingCamera != null) {
                reply.done(null, "camera capture is already in progress");
                return;
            }
            pendingCamera = mine;
        }
        if (Build.VERSION.SDK_INT >= 23 && act.checkSelfPermission(Manifest.permission.CAMERA) != PackageManager.PERMISSION_GRANTED) {
            MAIN.post(new Runnable() {
                @Override
                public void run() {
                    synchronized (LOCK) {
                        if (pendingCamera != mine || !isStillActive(act)) {
                            if (pendingCamera == mine) pendingCamera = null;
                            reply.done(null, "activity destroyed");
                            return;
                        }
                    }
                    try {
                        if (Build.VERSION.SDK_INT >= 23) {
                            act.requestPermissions(new String[]{Manifest.permission.CAMERA}, REQ_PERM_CAMERA);
                        }
                    } catch (Throwable t) {
                        synchronized (LOCK) {
                            if (pendingCamera == mine) pendingCamera = null;
                        }
                        reply.done(null, t.getMessage() != null ? t.getMessage() : "camera permission request failed");
                    }
                }
            });
            return;
        }
        launchCamera(act, mine);
    }

    private static void launchCamera(final Activity act, final PendingCamera mine) {
        MAIN.post(new Runnable() {
            @Override
            public void run() {
                synchronized (LOCK) {
                    if (pendingCamera != mine || !isStillActive(act)) {
                        if (pendingCamera == mine) pendingCamera = null;
                        mine.reply.done(null, "activity destroyed");
                        return;
                    }
                }
                try {
                    Intent intent = new Intent(MediaStore.ACTION_IMAGE_CAPTURE);
                    if ("front".equals(mine.facing)) {
                        intent.putExtra("android.intent.extras.CAMERA_FACING", 1);
                        intent.putExtra("android.intent.extras.LENS_FACING_FRONT", 1);
                    }
                    act.startActivityForResult(intent, REQ_CAMERA);
                } catch (Throwable t) {
                    synchronized (LOCK) {
                        if (pendingCamera == mine) pendingCamera = null;
                    }
                    mine.reply.done(null, t.getMessage() != null ? t.getMessage() : "camera unavailable");
                }
            }
        });
    }

    // ---- Activity callbacks ----

    public static boolean onActivityResult(Activity activity, int requestCode, int resultCode, final Intent data) {
        if (requestCode == REQ_FILE_PICK) {
            final PendingPick pick;
            synchronized (LOCK) {
                pick = pendingPick;
                pendingPick = null;
            }
            if (pick == null) return false;
            if (resultCode != Activity.RESULT_OK || data == null) {
                try {
                    JSONObject res = new JSONObject();
                    res.put("cancelled", true);
                    res.put("files", new JSONArray());
                    pick.reply.done(res, null);
                } catch (Throwable t) {
                    pick.reply.done(null, t.getMessage());
                }
                return true;
            }
            final List<Uri> uris = new ArrayList<>();
            ClipData clip = data.getClipData();
            if (clip != null && clip.getItemCount() > 0) {
                for (int i = 0; i < clip.getItemCount(); i++) {
                    Uri u = clip.getItemAt(i).getUri();
                    if (u != null) uris.add(u);
                }
            } else if (data.getData() != null) {
                uris.add(data.getData());
            }
            final Context appCtx = activity.getApplicationContext();
            IO.execute(new Runnable() {
                @Override
                public void run() {
                    try {
                        JSONArray files = new JSONArray();
                        int remaining = pick.maxBytes;
                        for (Uri uri : uris) {
                            byte[] raw = readUriBounded(appCtx, uri, remaining);
                            remaining -= raw.length;
                            String mime = appCtx.getContentResolver().getType(uri);
                            JSONObject entry = new JSONObject();
                            entry.put("name", queryDisplayName(appCtx, uri));
                            entry.put("mime", mime != null ? mime : "application/octet-stream");
                            entry.put("size", raw.length);
                            if ("base64".equals(pick.readAs)) {
                                entry.put("base64", Base64.encodeToString(raw, Base64.NO_WRAP));
                            } else {
                                entry.put("text", new String(raw, StandardCharsets.UTF_8));
                            }
                            files.put(entry);
                        }
                        JSONObject res = new JSONObject();
                        res.put("cancelled", files.length() == 0);
                        res.put("files", files);
                        pick.reply.done(res, null);
                    } catch (Throwable t) {
                        pick.reply.done(null, t.getMessage() != null ? t.getMessage() : "failed to read selected file");
                    }
                }
            });
            return true;
        }

        if (requestCode == REQ_FILE_SAVE) {
            final PendingSave save;
            synchronized (LOCK) {
                save = pendingSave;
                pendingSave = null;
            }
            if (save == null) return false;
            if (resultCode != Activity.RESULT_OK || data == null || data.getData() == null) {
                try {
                    JSONObject res = new JSONObject();
                    res.put("saved", false);
                    res.put("cancelled", true);
                    res.put("name", save.name);
                    save.reply.done(res, null);
                } catch (Throwable t) {
                    save.reply.done(null, t.getMessage());
                }
                return true;
            }
            final Uri target = data.getData();
            final Context appCtx = activity.getApplicationContext();
            IO.execute(new Runnable() {
                @Override
                public void run() {
                    try (OutputStream out = appCtx.getContentResolver().openOutputStream(target)) {
                        if (out == null) throw new IllegalStateException("cannot write to selected location");
                        out.write(save.data);
                        out.flush();
                        JSONObject res = new JSONObject();
                        res.put("saved", true);
                        res.put("cancelled", false);
                        res.put("name", save.name);
                        save.reply.done(res, null);
                    } catch (Throwable t) {
                        save.reply.done(null, t.getMessage() != null ? t.getMessage() : "failed to save file");
                    }
                }
            });
            return true;
        }

        if (requestCode == REQ_CAMERA) {
            final PendingCamera cam;
            synchronized (LOCK) {
                cam = pendingCamera;
                pendingCamera = null;
            }
            if (cam == null) return false;
            if (resultCode != Activity.RESULT_OK || data == null || data.getExtras() == null) {
                try {
                    JSONObject res = new JSONObject();
                    res.put("cancelled", true);
                    res.put("mime", "");
                    res.put("width", 0);
                    res.put("height", 0);
                    res.put("base64", "");
                    cam.reply.done(res, null);
                } catch (Throwable t) {
                    cam.reply.done(null, t.getMessage());
                }
                return true;
            }
            Object raw = data.getExtras().get("data");
            if (!(raw instanceof Bitmap)) {
                cam.reply.done(null, "camera did not return an image");
                return true;
            }
            final Bitmap bmp = (Bitmap) raw;
            IO.execute(new Runnable() {
                @Override
                public void run() {
                    try {
                        Bitmap scaled = bmp;
                        int w = bmp.getWidth();
                        int h = bmp.getHeight();
                        if (w <= 0 || h <= 0) {
                            cam.reply.done(null, "captured image has invalid dimensions");
                            return;
                        }
                        if (w > cam.maxDimension || h > cam.maxDimension) {
                            double scale = Math.min((double) cam.maxDimension / w, (double) cam.maxDimension / h);
                            int nw = Math.max(1, (int) Math.round(w * scale));
                            int nh = Math.max(1, (int) Math.round(h * scale));
                            scaled = Bitmap.createScaledBitmap(bmp, nw, nh, true);
                        }
                        ByteArrayOutputStream out = new ByteArrayOutputStream();
                        scaled.compress(Bitmap.CompressFormat.JPEG, cam.quality, out);
                        byte[] jpeg = out.toByteArray();
                        JSONObject res = new JSONObject();
                        res.put("cancelled", false);
                        res.put("mime", "image/jpeg");
                        res.put("width", scaled.getWidth());
                        res.put("height", scaled.getHeight());
                        res.put("base64", Base64.encodeToString(jpeg, Base64.NO_WRAP));
                        cam.reply.done(res, null);
                    } catch (Throwable t) {
                        cam.reply.done(null, t.getMessage() != null ? t.getMessage() : "failed to encode captured image");
                    }
                }
            });
            return true;
        }

        return false;
    }

    public static boolean onRequestPermissionsResult(Activity activity, int requestCode, String[] permissions, int[] grantResults) {
        if (requestCode == REQ_PERM_NOTIF) {
            TinyJump.Reply reply;
            synchronized (LOCK) {
                reply = pendingNotifPerm;
                pendingNotifPerm = null;
            }
            if (reply == null) return false;
            boolean granted = grantResults != null && grantResults.length > 0 && grantResults[0] == PackageManager.PERMISSION_GRANTED;
            try {
                JSONObject res = new JSONObject();
                res.put("granted", granted);
                res.put("status", granted ? "granted" : "denied");
                reply.done(res, null);
            } catch (Throwable t) {
                reply.done(null, t.getMessage());
            }
            return true;
        }

        if (requestCode == REQ_PERM_LOCATION) {
            PendingLocation loc;
            synchronized (LOCK) {
                loc = pendingLocation;
                pendingLocation = null;
                if (loc != null) locationInFlight = true;
            }
            if (loc == null) return false;
            boolean granted = false;
            if (grantResults != null) {
                for (int r : grantResults) {
                    if (r == PackageManager.PERMISSION_GRANTED) {
                        granted = true;
                        break;
                    }
                }
            }
            if (!granted) {
                synchronized (LOCK) { locationInFlight = false; }
                loc.reply.done(null, "location permission denied");
                return true;
            }
            fetchLocation(activity, loc.highAccuracy, loc.timeoutMs, loc.reply);
            return true;
        }

        if (requestCode == REQ_PERM_CAMERA) {
            PendingCamera cam;
            synchronized (LOCK) {
                cam = pendingCamera;
            }
            if (cam == null) return false;
            boolean granted = grantResults != null && grantResults.length > 0 && grantResults[0] == PackageManager.PERMISSION_GRANTED;
            if (!granted) {
                synchronized (LOCK) {
                    if (pendingCamera == cam) pendingCamera = null;
                }
                cam.reply.done(null, "camera permission denied");
                return true;
            }
            launchCamera(activity, cam);
            return true;
        }

        return false;
    }
}
