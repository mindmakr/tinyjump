plugins {
    id("com.android.library")
}

val repo: File = projectDir.resolve("../../..").canonicalFile
val tjAot: Boolean = project.findProperty("tjAot")?.toString()?.equals("true", ignoreCase = true) == true
val allowedAbis = setOf("armeabi-v7a", "arm64-v8a", "x86", "x86_64")
val tjAbis: List<String> = (project.findProperty("tjAbis")?.toString() ?: "armeabi-v7a,arm64-v8a,x86,x86_64")
    .split(",")
    .map { it.trim() }
    .filter { it.isNotEmpty() }
require(tjAbis.isNotEmpty() && tjAbis.all { it in allowedAbis }) {
    "invalid tjAbis=$tjAbis; expected comma-separated subset of $allowedAbis"
}
val rustLibs = layout.buildDirectory.dir(if (tjAot) "rust-aot/jniLibs" else "rust/jniLibs")

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

// Builds the Rust runtime (crates/tj-android) for the requested ABIs (default: all 4).
// Pass `-PtjAot=true` to strip the compiler (`--no-default-features`) for AOT `.tjb` apps.
val cargoNdk by tasks.registering(Exec::class) {
    workingDir = repo
    environment("ANDROID_HOME", android.sdkDirectory.absolutePath)
    inputs.property("tjAot", tjAot)
    inputs.property("tjAbis", tjAbis.joinToString(","))
    inputs.dir(repo.resolve("crates"))
    inputs.file(repo.resolve("Cargo.toml"))
    inputs.file(repo.resolve("Cargo.lock"))
    outputs.dir(rustLibs)
    doFirst {
        val outDir = rustLibs.get().asFile
        outDir.deleteRecursively()
        outDir.mkdirs()
    }
    val args = mutableListOf("cargo", "ndk", "--platform", "21")
    for (abi in tjAbis) {
        args += listOf("-t", abi)
    }
    args += listOf(
        "-o", rustLibs.get().asFile.absolutePath,
        "build", "--profile", "small", "-p", "tj-android",
    )
    if (tjAot) {
        args += listOf("--no-default-features", "--target-dir", repo.resolve("target/aot").absolutePath)
    }
    commandLine(args)
}

tasks.named("preBuild") { dependsOn(cargoNdk) }
