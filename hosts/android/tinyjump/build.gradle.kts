plugins {
    id("com.android.library")
}

val repo: File = rootDir.resolve("../..").canonicalFile
val rustLibs = layout.buildDirectory.dir("rust/jniLibs")

android {
    namespace = "com.mindmakr.tinyjump"
    compileSdk = 35

    defaultConfig {
        minSdk = 21
        consumerProguardFiles("consumer-rules.pro")
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_11
        targetCompatibility = JavaVersion.VERSION_11
    }

    sourceSets["main"].jniLibs.srcDir(rustLibs)
}

// Builds the Rust runtime (crates/tj-android) for every ABI old and new devices use.
val cargoNdk by tasks.registering(Exec::class) {
    workingDir = repo
    environment("ANDROID_HOME", android.sdkDirectory.absolutePath)
    commandLine(
        "cargo", "ndk", "--platform", "21",
        "-t", "armeabi-v7a", "-t", "arm64-v8a", "-t", "x86", "-t", "x86_64",
        "-o", rustLibs.get().asFile.absolutePath,
        "build", "--profile", "small", "-p", "tj-android",
    )
    inputs.dir(repo.resolve("crates"))
    inputs.file(repo.resolve("Cargo.lock"))
    outputs.dir(rustLibs)
}

tasks.named("preBuild") { dependsOn(cargoNdk) }
