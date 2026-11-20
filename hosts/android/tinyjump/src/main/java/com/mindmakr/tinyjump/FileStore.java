package com.mindmakr.tinyjump;

import android.util.AtomicFile;
import java.io.ByteArrayOutputStream;
import java.io.File;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.nio.charset.Charset;
import java.util.ArrayList;
import java.util.Collections;
import java.util.List;
import org.json.JSONArray;

/** Object storage: one JSON file per key, written atomically. */
final class FileStore {
    private static final Charset UTF8 = Charset.forName("UTF-8");
    private final File dir;

    FileStore(File dir) {
        this.dir = dir;
        //noinspection ResultOfMethodCallIgnored
        dir.mkdirs();
    }

    /**
     * Percent-encodes everything but [a-z0-9._-], so keys never become paths, and upper-case
     * letters too, so keys differing only in case stay apart on case-insensitive filesystems.
     */
    static String encode(String key) {
        StringBuilder out = new StringBuilder();
        for (byte b : key.getBytes(UTF8)) {
            int c = b & 0xff;
            if ((c >= 'a' && c <= 'z') || (c >= '0' && c <= '9') || c == '-' || c == '_' || c == '.') {
                out.append((char) c);
            } else {
                out.append('%').append(String.format("%02X", c));
            }
        }
        return out.toString();
    }

    static String decode(String name) {
        ByteArrayOutputStream out = new ByteArrayOutputStream();
        for (int i = 0; i < name.length(); i++) {
            char c = name.charAt(i);
            if (c == '%' && i + 2 < name.length()) {
                out.write(Integer.parseInt(name.substring(i + 1, i + 3), 16));
                i += 2;
            } else {
                out.write(c);
            }
        }
        return new String(out.toByteArray(), UTF8);
    }

    private AtomicFile file(String key) {
        return new AtomicFile(new File(dir, encode(key) + ".json"));
    }

    String load(String key) {
        try {
            return new String(file(key).readFully(), UTF8);
        } catch (IOException e) {
            return null;
        }
    }

    boolean save(String key, String value) {
        AtomicFile f = file(key);
        FileOutputStream out = null;
        try {
            out = f.startWrite();
            out.write(value.getBytes(UTF8));
            f.finishWrite(out);
            return true;
        } catch (IOException e) {
            if (out != null) f.failWrite(out);
            return false;
        }
    }

    /** Deletes a record; true when it is gone (or was never there). */
    boolean remove(String key) {
        file(key).delete();
        return !file(key).getBaseFile().exists();
    }

    /** JSON array of keys starting with prefix. */
    String keys(String prefix) {
        List<String> keys = new ArrayList<>();
        String[] names = dir.list();
        if (names != null) {
            for (String n : names) {
                if (!n.endsWith(".json")) continue;
                String key = decode(n.substring(0, n.length() - 5));
                if (key.startsWith(prefix)) keys.add(key);
            }
        }
        Collections.sort(keys);
        return new JSONArray(keys).toString();
    }

    static String readAll(InputStream in, int limit) throws IOException {
        ByteArrayOutputStream out = new ByteArrayOutputStream();
        byte[] buf = new byte[8192];
        int n;
        while ((n = in.read(buf)) > 0) {
            if (out.size() + n > limit) throw new IOException("response larger than " + limit + " bytes");
            out.write(buf, 0, n);
        }
        return new String(out.toByteArray(), UTF8);
    }
}
