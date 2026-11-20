package com.mindmakr.tinyjump;

/**
 * What the Rust runtime calls back into: storage and widget measurement. Method names and
 * signatures are part of the contract with crates/tj-android. Main thread only.
 */
final class Host {
    private final FileStore store;
    Renderer renderer;

    Host(FileStore store) {
        this.store = store;
    }

    String load(String key) {
        return store.load(key);
    }

    boolean save(String key, String value) {
        return store.save(key, value);
    }

    String keys(String prefix) {
        return store.keys(prefix);
    }

    boolean remove(String key) {
        return store.remove(key);
    }

    /** A leaf widget's natural size in dp, packed as two float bit patterns (width, height). */
    long measure(int node, float maxWidth) {
        return renderer == null ? 0 : renderer.measure(node, maxWidth);
    }
}
