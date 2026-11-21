package com.mindmakr.tinyjump;

import java.io.BufferedReader;
import java.io.IOException;
import java.io.InputStream;
import java.io.InputStreamReader;
import java.io.OutputStream;
import java.io.UnsupportedEncodingException;
import java.net.HttpURLConnection;
import java.net.URL;
import java.net.URLEncoder;
import java.util.ArrayList;
import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

/**
 * Server objects: calls `@server` methods over HTTP and keeps this device's replicas current
 * from the server's realtime stream. The stream resumes from a change number, so after a
 * reconnect the server sends only what this device missed.
 */
final class Sync {
    private static final int TIMEOUT_MS = 30000;
    private static final long RETRY_MS = 2000;

    private final TinyJump tj;
    /** Base URL of a `tj serve` backend, or null when the app has none. */
    private final String server;
    /** Object key -> change number of the state this device has. Main thread only. */
    private final Map<String, Long> replicas = new LinkedHashMap<>();
    private volatile int generation;
    private volatile HttpURLConnection stream;
    private volatile boolean stopped;
    /** Sent as `Authorization: Bearer` (docs/access-control.md); null when signed out. */
    private volatile String token;
    /** The signed-in user's id and roles. Main thread only. */
    private String user;
    private List<String> roles = Collections.emptyList();
    /** Told when the server answers 401, so the embedding app can refresh the token. */
    volatile Runnable onTokenExpired;

    /** The call got an answer, an error: retrying would not help. Anything else is unreachable. */
    static final class CallFailed extends Exception {
        CallFailed(String message) {
            super(message);
        }
    }

    Sync(TinyJump tj, String server) {
        this.tj = tj;
        this.server = server == null ? null : server.replaceAll("/+$", "");
    }

    /** Starts keeping {@code key} current; {@code seq} is the change number of its cached state. */
    void subscribe(String key, long seq) {
        if (server == null || replicas.containsKey(key)) return;
        // Already in a class stream (`Slot/*`): nothing new to ask for.
        if (!key.endsWith("/*") && replicas.containsKey(key.split("/", 2)[0] + "/*")) return;
        if (key.endsWith("/*")) {
            // The class stream covers the objects of the class asked for one by one.
            String prefix = key.substring(0, key.length() - 1);
            for (java.util.Iterator<String> i = replicas.keySet().iterator(); i.hasNext(); ) {
                if (i.next().startsWith(prefix)) i.remove();
            }
        }
        replicas.put(key, seq);
        connect(new ArrayList<>(replicas.keySet()), Collections.min(replicas.values()));
    }

    /**
     * Sets the token; returns true if the user or their roles changed, in which case replicas
     * start over (the runtime drops what the previous user queued, and resubscribes).
     */
    boolean setToken(String newToken, String newUser, List<String> newRoles) {
        token = newToken;
        boolean changed = (newUser == null ? user != null : !newUser.equals(user)) || !newRoles.equals(roles);
        user = newUser;
        roles = newRoles;
        if (changed) {
            replicas.clear();
            generation++;
            HttpURLConnection s = stream;
            if (s != null) s.disconnect();
        } else if (!replicas.isEmpty()) {
            connect(new ArrayList<>(replicas.keySet()), Collections.min(replicas.values()));
        }
        return changed;
    }

    private void authorize(HttpURLConnection c) {
        String t = token;
        if (t != null) c.setRequestProperty("Authorization", "Bearer " + t);
    }

    private void tokenExpired() {
        Runnable r = onTokenExpired;
        if (r != null) tj.main.post(r);
    }

    void stop() {
        stopped = true;
        HttpURLConnection s = stream;
        if (s != null) s.disconnect();
    }

    /**
     * Runs a server call; returns the method's result. The call id makes retries safe: the
     * server runs each call once. Called on an io thread.
     */
    Object call(JSONObject effect) throws Exception {
        if (server == null) throw new CallFailed("no server configured for " + effect.getString("object"));
        String url = objectUrl(effect.getString("object")) + "/" + encode(effect.getString("method"));
        HttpURLConnection c = open(url);
        try {
            c.setRequestMethod("POST");
            c.setDoOutput(true);
            c.setRequestProperty("Content-Type", "application/json");
            c.setRequestProperty("Idempotency-Key", effect.getString("call"));
            authorize(c);
            JSONArray args = effect.getJSONArray("args");
            try (OutputStream out = c.getOutputStream()) {
                out.write(args.toString().getBytes("UTF-8"));
            }
            int status = c.getResponseCode();
            // A 401 is retried too, once the embedding app has refreshed the token.
            if (status == 401) tokenExpired();
            if (status >= 500 || status == 401) throw new IOException("server answered " + status);
            JSONObject reply = new JSONObject(body(c));
            if (reply.has("error")) throw new CallFailed(reply.getString("error"));
            return reply.opt("ok");
        } finally {
            c.disconnect();
        }
    }

    private String objectUrl(String key) throws UnsupportedEncodingException {
        StringBuilder sb = new StringBuilder(server).append("/api");
        for (String part : key.split("/")) sb.append('/').append(encode(part));
        return sb.toString();
    }

    private static String encode(String s) throws UnsupportedEncodingException {
        return URLEncoder.encode(s, "UTF-8").replace("+", "%20");
    }

    private static HttpURLConnection open(String url) throws IOException {
        HttpURLConnection c = (HttpURLConnection) new URL(url).openConnection();
        c.setConnectTimeout(TIMEOUT_MS);
        c.setReadTimeout(TIMEOUT_MS);
        return c;
    }

    private static String body(HttpURLConnection c) throws IOException {
        InputStream in = c.getResponseCode() >= 400 ? c.getErrorStream() : c.getInputStream();
        return in == null ? "{}" : FileStore.readAll(in, 10 << 20);
    }

    private void online() {
        tj.main.post(new Runnable() {
            @Override
            public void run() {
                tj.send(TinyJump.input("online"));
            }
        });
    }

    private void deliver(final String key, final Object state, final long seq) {
        tj.main.post(new Runnable() {
            @Override
            public void run() {
                // An object of a class streamed whole (`Slot/*`): the class's entry keeps the
                // change number, so a reconnect resumes and the stream address stays short.
                String whole = key.split("/", 2)[0] + "/*";
                String entry = replicas.containsKey(whole) ? whole : key;
                Long have = replicas.get(entry);
                if (have == null) return;
                replicas.put(entry, Math.max(have, seq));
                try {
                    tj.send(TinyJump.input("sync").put("object", key).put("state", state).put("seq", seq));
                } catch (JSONException e) {
                    throw new IllegalStateException(e);
                }
            }
        });
    }

    /**
     * One stream for all replicas; a newer generation replaces it when the set grows. It starts
     * after change number {@code since} and resumes from the last change it delivered.
     */
    private void connect(final List<String> keys, final long since) {
        final int gen = ++generation;
        HttpURLConnection old = stream;
        if (old != null) old.disconnect();
        new Thread(new Runnable() {
            @Override
            public void run() {
                long cursor = since;
                while (!stopped && gen == generation) {
                    try {
                        StringBuilder url = new StringBuilder(server).append("/api/events?");
                        for (String k : keys) url.append("object=").append(encode(k)).append('&');
                        url.append("since=").append(cursor);
                        HttpURLConnection c = (HttpURLConnection) new URL(url.toString()).openConnection();
                        c.setConnectTimeout(TIMEOUT_MS);
                        c.setReadTimeout(0); // the server sends keep-alives; wait indefinitely
                        authorize(c);
                        stream = c;
                        if (c.getResponseCode() == 401) tokenExpired();
                        BufferedReader r = new BufferedReader(new InputStreamReader(c.getInputStream(), "UTF-8"));
                        online(); // reachable again: queued calls go now, not after their backoff
                        String event = null;
                        for (String line; (line = r.readLine()) != null && gen == generation; ) {
                            if (line.startsWith("event: ")) {
                                event = line.substring(7);
                            } else if (line.startsWith("data: ") && "changed".equals(event)) {
                                JSONObject d = new JSONObject(line.substring(6));
                                long seq = d.getLong("seq");
                                cursor = Math.max(cursor, seq); // the stream is in change order
                                deliver(d.getString("object"), d.get("state"), seq);
                            } else if (line.isEmpty()) {
                                event = null;
                            }
                        }
                    } catch (Exception e) {
                        // Disconnected or offline: retry below.
                    }
                    if (stopped || gen != generation) return;
                    try {
                        Thread.sleep(RETRY_MS);
                    } catch (InterruptedException e) {
                        return;
                    }
                }
            }
        }, "tj-sync").start();
    }
}
