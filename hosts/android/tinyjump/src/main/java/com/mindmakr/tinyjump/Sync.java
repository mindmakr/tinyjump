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

    Sync(TinyJump tj, String server) {
        this.tj = tj;
        this.server = server == null ? null : server.replaceAll("/+$", "");
    }

    /** Starts keeping {@code key} current; {@code seq} is the change number of its cached state. */
    void subscribe(String key, long seq) {
        if (server == null || replicas.containsKey(key)) return;
        replicas.put(key, seq);
        connect(new ArrayList<>(replicas.keySet()), Collections.min(replicas.values()));
    }

    void stop() {
        stopped = true;
        HttpURLConnection s = stream;
        if (s != null) s.disconnect();
    }

    /** Runs a server call; returns the method's result. Called on an io thread. */
    Object call(JSONObject effect) throws Exception {
        if (server == null) throw new IOException("no server configured for " + effect.getString("object"));
        String url = objectUrl(effect.getString("object")) + "/" + encode(effect.getString("method"));
        HttpURLConnection c = open(url);
        try {
            c.setRequestMethod("POST");
            c.setDoOutput(true);
            c.setRequestProperty("Content-Type", "application/json");
            JSONArray args = effect.getJSONArray("args");
            try (OutputStream out = c.getOutputStream()) {
                out.write(args.toString().getBytes("UTF-8"));
            }
            JSONObject reply = new JSONObject(body(c));
            if (reply.has("error")) throw new IOException(reply.getString("error"));
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

    private void deliver(final String key, final Object state, final long seq) {
        tj.main.post(new Runnable() {
            @Override
            public void run() {
                Long have = replicas.get(key);
                if (have == null) return;
                replicas.put(key, Math.max(have, seq));
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
                        stream = c;
                        BufferedReader r = new BufferedReader(new InputStreamReader(c.getInputStream(), "UTF-8"));
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
