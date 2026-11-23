package com.mindmakr.tinyjump;

import android.content.Context;
import android.os.Handler;
import android.os.Looper;
import android.util.Log;
import android.view.View;
import android.view.ViewGroup;
import java.io.File;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.HttpURLConnection;
import java.net.URL;
import java.util.ArrayList;
import java.util.Collections;
import java.util.HashMap;
import java.util.Iterator;
import java.util.List;
import java.util.Map;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

/**
 * Runs a tinyjump app on Android: boots the runtime, draws its view tree with native
 * Views, performs effects, wakes it for timers, and (in debug builds) serves automation.
 * Use from the main thread only.
 */
public final class TinyJump {
    /** Receives the reply to a request: {@code ok} on success, otherwise {@code error}. */
    public interface Reply {
        void done(Object ok, String error);
    }

    static final String TAG = "tinyjump";
    private static final int MAX_RESPONSE = 10 << 20;
    private static final int HTTP_TIMEOUT_MS = 30000;
    private static final Map<String, NativeComponent> COMPONENTS = new HashMap<>();

    /**
     * Registers a custom native view used from {@code .tj} as {@code <Native name="..." />}.
     * Call before {@link #start} so the initial render finds it.
     */
    public static synchronized void registerComponent(String name, NativeComponent component) {
        if (component == null) COMPONENTS.remove(name); else COMPONENTS.put(name, component);
    }

    static synchronized NativeComponent nativeComponent(String name) {
        return COMPONENTS.get(name);
    }

    final Handler main = new Handler(Looper.getMainLooper());
    final ExecutorService io = Executors.newFixedThreadPool(4);
    final Renderer renderer;
    private final Sync sync;
    private final Map<Long, Reply> pending = new HashMap<>();
    private long nextReply = 1;
    private Automation automation;
    private final Runnable tick = new Runnable() {
        @Override
        public void run() {
            send(input("tick"));
        }
    };

    private TinyJump(Context context, ViewGroup root, String server) {
        renderer = new Renderer(context, root, this);
        sync = new Sync(this, server);
        // A new screen size means a new layout.
        renderer.root.addOnLayoutChangeListener(new View.OnLayoutChangeListener() {
            @Override
            public void onLayoutChange(View v, int l, int t, int r, int b, int ol, int ot, int or, int ob) {
                if (r - l != or - ol || b - t != ob - ot) layout();
            }
        });
    }

    /** Boots {@code source} into {@code root}. Throws with the compiler's message if it does not compile. */
    public static TinyJump start(Context context, ViewGroup root, String source) {
        return start(context, root, source, null);
    }

    /**
     * Like {@link #start(Context, ViewGroup, String)}, with the base URL of a `tj serve` backend
     * that hosts the app's `@server` objects (null for none).
     */
    public static TinyJump start(Context context, ViewGroup root, String source, String server) {
        assertMain();
        TinyJump tj = new TinyJump(context, root, server);
        Host host = new Host(new FileStore(new File(context.getFilesDir(), "tinyjump")));
        host.renderer = tj.renderer;
        java.security.SecureRandom random = new java.security.SecureRandom();
        // The seed drives Math.random; ids and crypto.randomId() use the `entropy` bytes below.
        int code = Native.boot(host, source, now(), random.nextLong(), true);
        String out = Native.out();
        if (code != 0) throw new IllegalStateException(describeFailure(code, out));
        tj.apply(out);
        byte[] bytes = new byte[32];
        random.nextBytes(bytes);
        StringBuilder hex = new StringBuilder();
        for (byte b : bytes) hex.append(String.format("%02x", b & 0xff));
        try {
            tj.send(input("entropy").put("bytes", hex.toString()));
        } catch (JSONException e) {
            throw new IllegalStateException(e);
        }
        return tj;
    }

    /**
     * The signed-in user's token for the server, their id and their roles as the server reports
     * them ({@code GET /api/whoami}; the roles only choose what the UI shows). Call it right
     * after {@link #start} and whenever the token is refreshed. A new user id or new roles drop
     * the server calls the previous identity queued and the replicas it saw
     * (docs/access-control.md).
     */
    public void setToken(String token, String userId, List<String> roles) {
        assertMain();
        List<String> held = roles == null ? Collections.<String>emptyList() : new ArrayList<>(roles);
        if (sync.setToken(token, userId, held)) {
            try {
                send(input("identity").put("user", userId == null ? JSONObject.NULL : userId).put("roles", new JSONArray(held)));
            } catch (JSONException e) {
                throw new IllegalStateException(e);
            }
        }
    }

    /** Forgets the token, the replicas and every queued server call. */
    public void signOut() {
        setToken(null, null, null);
    }

    /** Runs on the main thread when the server answers 401: refresh the token, then setToken. */
    public void setOnTokenExpired(Runnable listener) {
        sync.onTokenExpired = listener;
    }

    /** Serves /_tj/inspect and /_tj/input on 127.0.0.1:port. Reach it with `adb forward tcp:port tcp:port`. */
    public void enableAutomation(int port) {
        if (automation == null) {
            automation = new Automation(this, port);
            automation.start();
        }
    }

    public void stop() {
        main.removeCallbacks(tick);
        sync.stop();
        io.shutdownNow();
        if (automation != null) automation.stop();
        renderer.destroy();
    }

    /** Sends an input that expects a reply (call, get, ui, inspect). */
    public void request(JSONObject input, Reply reply) {
        long id = nextReply++;
        pending.put(id, reply);
        try {
            input.put("reply", id);
        } catch (JSONException e) {
            throw new IllegalArgumentException(e);
        }
        send(input);
    }

    void send(JSONObject input) {
        assertMain();
        int code = Native.handle(now(), input.toString());
        String out = Native.out();
        if (code != 0) {
            Log.e(TAG, "rejected input: " + out);
            return;
        }
        apply(out);
    }

    /** Asks the core to lay the screen out at the root's current size, mirrored for RTL locales. */
    void layout() {
        float[] size = renderer.viewportDp();
        if (size == null) return;
        boolean rtl = renderer.root.getLayoutDirection() == View.LAYOUT_DIRECTION_RTL;
        try {
            send(input("layout").put("width", size[0]).put("height", size[1]).put("rtl", rtl));
        } catch (JSONException e) {
            throw new IllegalStateException(e);
        }
    }

    static JSONObject input(String type) {
        try {
            return new JSONObject().put("type", type);
        } catch (JSONException e) {
            throw new IllegalStateException(e);
        }
    }

    static double now() {
        return System.currentTimeMillis();
    }

    private static void assertMain() {
        if (Looper.myLooper() != Looper.getMainLooper()) throw new IllegalStateException("tinyjump must be used on the main thread");
    }

    private static String describeFailure(int code, String out) {
        try {
            if (code == 1 && out.startsWith("[")) {
                JSONArray errors = new JSONArray(out);
                StringBuilder sb = new StringBuilder();
                for (int i = 0; i < errors.length(); i++) sb.append(errors.getJSONObject(i).optString("text")).append("\n\n");
                return sb.toString().trim();
            }
            JSONObject o = new JSONObject(out);
            return o.optString("message") + (o.has("trace") ? "\n" + o.getJSONArray("trace").join("\n") : "");
        } catch (JSONException e) {
            return out;
        }
    }

    private void apply(String outputs) {
        boolean relayout = false;
        try {
            JSONArray list = new JSONArray(outputs);
            for (int i = 0; i < list.length(); i++) {
                JSONObject o = list.getJSONObject(i);
                switch (o.getString("type")) {
                    case "patch":
                        relayout |= renderer.apply(o.getJSONArray("ops"));
                        break;
                    case "frames":
                        renderer.frames(o.getJSONArray("frames"));
                        break;
                    case "effect":
                        runEffect(o.getInt("id"), o.getJSONObject("effect"));
                        break;
                    case "subscribe":
                        sync.subscribe(o.getString("object"), o.optLong("seq"));
                        break;
                    case "wake":
                        main.removeCallbacks(tick);
                        if (!o.isNull("at")) main.postDelayed(tick, Math.max(0, (long) (o.getDouble("at") - now())));
                        break;
                    case "reply": {
                        Reply r = pending.remove(o.getLong("reply"));
                        if (r != null) r.done(o.opt("ok"), o.has("error") ? o.getString("error") : null);
                        break;
                    }
                    case "log":
                        Log.println(level(o.getString("level")), TAG, o.getString("message"));
                        break;
                    case "error":
                        Log.e(TAG, o.getString("message") + "\n  " + o.getJSONArray("trace").join("\n  "));
                        break;
                    default:
                        break; // emit / changed: no listeners on this host yet
                }
            }
        } catch (JSONException e) {
            Log.e(TAG, "bad output from runtime", e);
        }
        if (relayout) layout();
    }

    private static int level(String level) {
        return "error".equals(level) ? Log.ERROR : "warn".equals(level) ? Log.WARN : Log.INFO;
    }

    private void runEffect(final int id, final JSONObject effect) {
        io.execute(new Runnable() {
            @Override
            public void run() {
                Object ok = null;
                String error = null;
                String field = null;
                boolean call = "call".equals(effect.optString("kind"));
                boolean unreachable = false;
                try {
                    ok = call ? sync.call(effect) : http(effect);
                } catch (Exception e) {
                    error = String.valueOf(e.getMessage() != null ? e.getMessage() : e);
                    if (e instanceof Sync.CallFailed) field = ((Sync.CallFailed) e).field;
                    // A server call without an answer is retried by the runtime.
                    unreachable = call && !(e instanceof Sync.CallFailed);
                }
                final JSONObject done = input("effectDone");
                try {
                    done.put("id", id);
                    if (error == null) {
                        done.put("ok", ok == null ? JSONObject.NULL : ok);
                    } else {
                        done.put("error", error);
                        if (field != null) done.put("field", field);
                    }
                    if (unreachable) done.put("unreachable", true);
                } catch (JSONException e) {
                    throw new IllegalStateException(e);
                }
                main.post(new Runnable() {
                    @Override
                    public void run() {
                        send(done);
                    }
                });
            }
        });
    }

    private static JSONObject http(JSONObject effect) throws Exception {
        if (!"http".equals(effect.getString("kind"))) throw new IllegalArgumentException("unsupported effect " + effect.getString("kind"));
        String url = effect.getString("url");
        if (!url.startsWith("https://") && !url.startsWith("http://")) throw new IllegalArgumentException("only http(s) URLs can be fetched");
        HttpURLConnection c = (HttpURLConnection) new URL(url).openConnection();
        try {
            c.setRequestMethod(effect.getString("method"));
            c.setConnectTimeout(HTTP_TIMEOUT_MS);
            c.setReadTimeout(HTTP_TIMEOUT_MS);
            JSONObject headers = effect.optJSONObject("headers");
            if (headers != null) {
                for (Iterator<String> it = headers.keys(); it.hasNext(); ) {
                    String k = it.next();
                    c.setRequestProperty(k, headers.getString(k));
                }
            }
            if (effect.has("body") && !effect.isNull("body")) {
                c.setDoOutput(true);
                try (OutputStream out = c.getOutputStream()) {
                    out.write(effect.getString("body").getBytes("UTF-8"));
                }
            }
            int status = c.getResponseCode();
            InputStream in = status >= 400 ? c.getErrorStream() : c.getInputStream();
            String body = in == null ? "" : FileStore.readAll(in, MAX_RESPONSE);
            JSONObject h = new JSONObject();
            for (Map.Entry<String, List<String>> e : c.getHeaderFields().entrySet()) {
                if (e.getKey() != null && !e.getValue().isEmpty()) h.put(e.getKey().toLowerCase(), e.getValue().get(0));
            }
            return new JSONObject().put("status", status).put("headers", h).put("body", body);
        } finally {
            c.disconnect();
        }
    }
}
