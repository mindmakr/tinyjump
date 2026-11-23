package com.mindmakr.tinyjump;

import android.util.Log;
import java.io.BufferedInputStream;
import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.InetAddress;
import java.net.ServerSocket;
import java.net.Socket;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicReference;
import org.json.JSONException;
import org.json.JSONObject;

/**
 * Debug automation on 127.0.0.1, the same endpoints as `tj serve`:
 * GET /_tj/inspect (semantic tree with dp bounds + object state) and POST /_tj/input (one protocol input).
 */
final class Automation {
    private static final int MAX_BODY = 1 << 20;
    private static final long TIMEOUT_S = 30;

    private final TinyJump tj;
    private final int port;
    private volatile ServerSocket server;

    Automation(TinyJump tj, int port) {
        this.tj = tj;
        this.port = port;
    }

    void start() {
        new Thread(new Runnable() {
            @Override
            public void run() {
                try (ServerSocket s = new ServerSocket(port, 8, InetAddress.getByName("127.0.0.1"))) {
                    server = s;
                    Log.i(TinyJump.TAG, "automation on 127.0.0.1:" + port);
                    while (!s.isClosed()) {
                        final Socket client = s.accept();
                        new Thread(new Runnable() {
                            @Override
                            public void run() {
                                serve(client);
                            }
                        }, "tj-automation-client").start();
                    }
                } catch (IOException e) {
                    if (server == null || !server.isClosed()) Log.w(TinyJump.TAG, "automation stopped: " + e);
                }
            }
        }, "tj-automation").start();
    }

    void stop() {
        try {
            if (server != null) server.close();
        } catch (IOException ignored) {
            // closing anyway
        }
    }

    private void serve(Socket client) {
        try (Socket s = client) {
            s.setSoTimeout((int) TimeUnit.SECONDS.toMillis(TIMEOUT_S));
            InputStream in = new BufferedInputStream(s.getInputStream());
            String[] request = readLine(in).split(" ");
            int length = 0;
            for (String line = readLine(in); !line.isEmpty(); line = readLine(in)) {
                int colon = line.indexOf(':');
                if (colon > 0 && line.substring(0, colon).trim().equalsIgnoreCase("content-length")) {
                    length = Integer.parseInt(line.substring(colon + 1).trim());
                }
            }
            if (length < 0 || length > MAX_BODY) {
                respond(s.getOutputStream(), 413, error("body too large"));
                return;
            }
            byte[] body = new byte[length];
            for (int read = 0; read < length; ) {
                int n = in.read(body, read, length - read);
                if (n < 0) throw new IOException("short body");
                read += n;
            }
            String method = request.length > 0 ? request[0] : "";
            String path = request.length > 1 ? request[1] : "";
            JSONObject result;
            if ("GET".equals(method) && "/_tj/screenshot".equals(path)) {
                byte[] png = screenshot();
                if (png == null) {
                    respond(s.getOutputStream(), 500, error("nothing to capture yet"));
                } else {
                    respond(s.getOutputStream(), 200, "image/png", png);
                }
                return;
            } else if ("GET".equals(method) && "/_tj/inspect".equals(path)) {
                result = onMain(TinyJump.input("inspect"), true);
            } else if ("POST".equals(method) && "/_tj/input".equals(path)) {
                JSONObject input = new JSONObject(new String(body, "UTF-8"));
                String type = input.optString("type");
                boolean replies = "call".equals(type) || "get".equals(type) || "ui".equals(type) || "inspect".equals(type);
                result = onMain(input, replies);
            } else {
                respond(s.getOutputStream(), 404, error("not found"));
                return;
            }
            respond(s.getOutputStream(), result.has("error") ? 400 : 200, result);
        } catch (Exception e) {
            Log.w(TinyJump.TAG, "automation request failed: " + e);
        }
    }

    /** Runs the input on the main thread and waits for its reply. */
    private JSONObject onMain(final JSONObject input, final boolean replies) throws InterruptedException, JSONException {
        final CountDownLatch done = new CountDownLatch(1);
        final AtomicReference<JSONObject> out = new AtomicReference<>();
        tj.main.post(new Runnable() {
            @Override
            public void run() {
                try {
                    tj.renderer.forAgent(input);
                } catch (JSONException e) {
                    throw new IllegalStateException(e);
                }
                if (!replies) {
                    if ("locale".equals(input.optString("type"))) {
                        tj.setLocale(input.optString("locale", ""));
                    } else {
                        tj.send(input);
                    }
                    out.set(ok(JSONObject.NULL));
                    done.countDown();
                    return;
                }
                tj.request(input, new TinyJump.Reply() {
                    @Override
                    public void done(Object ok, String err) {
                        try {
                            if (err != null) {
                                out.set(error(err));
                            } else {
                                if ("inspect".equals(input.optString("type")) && ok instanceof JSONObject) {
                                    tj.renderer.addBounds(((JSONObject) ok).getJSONArray("tree"));
                                    ((JSONObject) ok).put("viewport", tj.renderer.viewport());
                                }
                                out.set(ok(ok));
                            }
                        } catch (JSONException e) {
                            out.set(error(e.toString()));
                        }
                        done.countDown();
                    }
                });
            }
        });
        if (!done.await(TIMEOUT_S, TimeUnit.SECONDS)) return error("timed out waiting for the app");
        return out.get();
    }

    private static JSONObject ok(Object value) {
        try {
            return new JSONObject().put("ok", value == null ? JSONObject.NULL : value);
        } catch (JSONException e) {
            throw new IllegalStateException(e);
        }
    }

    private static JSONObject error(String message) {
        try {
            return new JSONObject().put("error", message);
        } catch (JSONException e) {
            throw new IllegalStateException(e);
        }
    }

    private static String readLine(InputStream in) throws IOException {
        ByteArrayOutputStream line = new ByteArrayOutputStream();
        for (int c = in.read(); c >= 0 && c != '\n'; c = in.read()) {
            if (c != '\r') line.write(c);
            if (line.size() > 8192) throw new IOException("header line too long");
        }
        return line.toString("UTF-8");
    }

    /** Captures the window on the main thread. */
    private byte[] screenshot() throws InterruptedException {
        final CountDownLatch done = new CountDownLatch(1);
        final AtomicReference<byte[]> png = new AtomicReference<>();
        tj.main.post(new Runnable() {
            @Override
            public void run() {
                png.set(tj.renderer.screenshot());
                done.countDown();
            }
        });
        done.await(TIMEOUT_S, TimeUnit.SECONDS);
        return png.get();
    }

    private static void respond(OutputStream out, int status, JSONObject body) throws IOException {
        respond(out, status, "application/json", body.toString().getBytes("UTF-8"));
    }

    private static void respond(OutputStream out, int status, String type, byte[] bytes) throws IOException {
        String head = "HTTP/1.1 " + status + (status == 200 ? " OK" : " Error") + "\r\nContent-Type: " + type + "\r\nContent-Length: " + bytes.length + "\r\nConnection: close\r\n\r\n";
        out.write(head.getBytes("UTF-8"));
        out.write(bytes);
        out.flush();
    }
}
