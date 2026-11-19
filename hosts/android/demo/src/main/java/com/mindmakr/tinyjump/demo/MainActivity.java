package com.mindmakr.tinyjump.demo;

import android.app.Activity;
import android.content.pm.ApplicationInfo;
import android.os.Bundle;
import android.view.View;
import android.widget.FrameLayout;
import com.mindmakr.tinyjump.TinyJump;
import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.util.ArrayList;
import java.util.Collections;
import java.util.List;

/**
 * Runs one of the repo's example apps. Launch extras: {@code example} (default "todo"),
 * {@code dir} ("rtl" or "ltr", overriding the locale) and {@code server} (a `tj serve`/`tj dev` URL for @server objects, e.g. http://10.0.2.2:4700).
 * Debug builds serve automation on port 4701.
 */
public final class MainActivity extends Activity {
    private static final int AUTOMATION_PORT = 4701;
    private TinyJump app;

    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        FrameLayout root = new FrameLayout(this);
        root.setFitsSystemWindows(true);
        // `dir` overrides the locale's direction ("rtl"/"ltr"), e.g. to test Arabic or Urdu layouts.
        String dir = getIntent().getStringExtra("dir");
        if ("rtl".equals(dir)) root.setLayoutDirection(View.LAYOUT_DIRECTION_RTL);
        if ("ltr".equals(dir)) root.setLayoutDirection(View.LAYOUT_DIRECTION_LTR);
        setContentView(root);
        String example = getIntent().getStringExtra("example");
        String source = source(example == null ? "todo" : example);
        app = TinyJump.start(this, root, source, getIntent().getStringExtra("server"));
        if ((getApplicationInfo().flags & ApplicationInfo.FLAG_DEBUGGABLE) != 0) app.enableAutomation(AUTOMATION_PORT);
    }

    @Override
    protected void onDestroy() {
        if (app != null) app.stop();
        super.onDestroy();
    }

    /**
     * The example's files as one source: app.tj first, then its other .tj files, each after a
     * {@code //# file: <path>} line (the bundle format, see tj-core's modules.rs).
     */
    private String source(String example) {
        List<String> names = new ArrayList<>();
        collect(example, "", names);
        Collections.sort(names);
        names.remove("app.tj");
        if (names.isEmpty()) return asset(example + "/app.tj");
        names.add(0, "app.tj");
        StringBuilder out = new StringBuilder();
        for (String name : names) {
            String text = asset(example + "/" + name);
            out.append("//# file: ").append(name).append('\n').append(text);
            if (!text.endsWith("\n")) out.append('\n');
        }
        return out.toString();
    }

    private void collect(String dir, String prefix, List<String> out) {
        String[] children;
        try {
            children = getAssets().list(dir);
        } catch (IOException e) {
            return;
        }
        if (children == null) return;
        for (String child : children) {
            if (child.endsWith(".tj")) out.add(prefix + child);
            else collect(dir + "/" + child, prefix + child + "/", out);
        }
    }

    private String asset(String name) {
        try (InputStream in = getAssets().open(name)) {
            ByteArrayOutputStream out = new ByteArrayOutputStream();
            byte[] buf = new byte[8192];
            for (int n; (n = in.read(buf)) > 0; ) out.write(buf, 0, n);
            return out.toString("UTF-8");
        } catch (IOException e) {
            throw new IllegalStateException("missing asset " + name, e);
        }
    }
}
