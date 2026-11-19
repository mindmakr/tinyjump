package com.mindmakr.tinyjump.demo;

import android.app.Activity;
import android.content.pm.ApplicationInfo;
import android.os.Bundle;
import android.widget.FrameLayout;
import com.mindmakr.tinyjump.TinyJump;
import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;

/** Runs examples/todo/app.tj. Debug builds serve automation on port 4701. */
public final class MainActivity extends Activity {
    private static final int AUTOMATION_PORT = 4701;
    private TinyJump app;

    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        FrameLayout root = new FrameLayout(this);
        root.setFitsSystemWindows(true);
        setContentView(root);
        app = TinyJump.start(this, root, asset("app.tj"));
        if ((getApplicationInfo().flags & ApplicationInfo.FLAG_DEBUGGABLE) != 0) app.enableAutomation(AUTOMATION_PORT);
    }

    @Override
    protected void onDestroy() {
        if (app != null) app.stop();
        super.onDestroy();
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
