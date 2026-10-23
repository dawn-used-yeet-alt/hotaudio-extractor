package net.hotaudio.extractor

import android.content.Context

/**
 * The one JNI hand-off UniFFI cannot generate.
 *
 * `rustls-platform-verifier` on Android verifies certificates through the JVM's
 * `X509TrustManager`, so it needs the application `Context` once before the
 * first HTTPS request. Call [initAndroidTls] from `Application.onCreate` (or
 * from the first component that can download); it is idempotent.
 *
 * The generated Kotlin API and this object must load the same native library:
 * `jniLibs/<abi>/libhotaudio_ffi.so`, built by `cargo ndk`.
 */
object Hotaudio {
    init {
        System.loadLibrary("hotaudio_ffi")
    }

    /**
     * Initialize the platform TLS verifier.
     *
     * @param context an application context, retained for the process lifetime.
     */
    @JvmStatic
    external fun initAndroidTls(context: Context)
}
