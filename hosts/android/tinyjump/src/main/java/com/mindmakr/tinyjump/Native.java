package com.mindmakr.tinyjump;

/** JNI bridge to the Rust runtime (crates/tj-android). Main thread only. */
final class Native {
    static {
        System.loadLibrary("tj_android");
    }

    private Native() {}

    /** Compiles and boots; status codes as in tj_core::host. The payload is in {@link #out()}. */
    static native int boot(FileStore store, String source, double now, long seed, boolean render);

    /** Handles one JSON input; the payload is in {@link #out()}. */
    static native int handle(double now, String input);

    /** The JSON payload of the last call. */
    static native String out();
}
