# Called from Rust through JNI by name.
-keep class com.mindmakr.tinyjump.Host { *; }
-keepclasseswithmembernames class com.mindmakr.tinyjump.Native { native <methods>; }
