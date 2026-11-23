package com.mindmakr.tinyjump;

import android.content.Context;
import org.json.JSONObject;

/**
 * Native device capability plugin registered via {@link TinyJump#registerPlugin(String, DevicePlugin)}
 * and invoked from `.tj` plugin modules via {@code plugin(name, method, args)} (docs/device-plugins.md).
 *
 * Implementations may run work asynchronously and invoke {@code reply.done(ok, error)} from any
 * thread; {@link TinyJump} delivers the first reply back onto the main thread and ignores duplicates.
 */
public interface DevicePlugin {
    void call(Context context, String method, JSONObject args, TinyJump.Reply reply) throws Exception;
}
