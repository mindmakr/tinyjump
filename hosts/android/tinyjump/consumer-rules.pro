# Called from Rust through JNI by name.
-keep class com.mindmakr.tinyjump.FileStore { *; }
-keepclasseswithmembernames class com.mindmakr.tinyjump.Native { native <methods>; }
