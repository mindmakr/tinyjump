plugins {
    id("com.android.application")
}

android {
    namespace = "com.mindmakr.tinyjump.demo"
    compileSdk = 35

    defaultConfig {
        applicationId = "com.mindmakr.tinyjump.demo"
        minSdk = 21
        targetSdk = 35
        versionCode = 1
        versionName = "0.1.0"
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_11
        targetCompatibility = JavaVersion.VERSION_11
    }

    // The app source comes straight from the repo's example, not a copy.
    sourceSets["main"].assets.srcDir(rootDir.resolve("../../examples/todo"))
}

dependencies {
    implementation(project(":tinyjump"))
}
