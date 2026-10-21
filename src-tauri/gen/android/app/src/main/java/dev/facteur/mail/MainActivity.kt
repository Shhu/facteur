package dev.facteur.mail

import android.os.Bundle
import android.webkit.WebView
import androidx.activity.enableEdgeToEdge
import androidx.webkit.WebViewCompat
import androidx.webkit.WebViewFeature
import org.json.JSONObject

class MainActivity : TauriActivity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
    if (MailListenerService.isEnabled(this)) MailListenerService.start(this)
  }

  override fun onWebViewCreate(webView: WebView) {
    super.onWebViewCreate(webView)
    // Background listening switch, read and set from Settings (window.FacteurAndroid).
    //
    // A web message listener rather than addJavascriptInterface: the object is only injected
    // into frames whose origin is the app's own, never into a page the web view might end up
    // on, and messages are checked to come from the main frame. Requests are JSON strings:
    //   {"id": 1, "op": "get"}                     → {"id": 1, "enabled": true}
    //   {"id": 2, "op": "set", "enabled": false}   → {"id": 2, "enabled": false}
    if (!WebViewFeature.isFeatureSupported(WebViewFeature.WEB_MESSAGE_LISTENER)) return
    WebViewCompat.addWebMessageListener(webView, "FacteurAndroid", allowedOrigins()) { _, message, _, isMainFrame, reply ->
      if (!isMainFrame) return@addWebMessageListener
      val request = try {
        JSONObject(message.data ?: return@addWebMessageListener)
      } catch (e: Exception) {
        return@addWebMessageListener
      }
      when (request.optString("op")) {
        "get" -> {}
        "set" -> MailListenerService.setEnabled(this, request.optBoolean("enabled"))
        else -> return@addWebMessageListener
      }
      val answer = JSONObject().put("id", request.opt("id")).put("enabled", MailListenerService.isEnabled(this))
      reply.postMessage(answer.toString())
    }
  }

  /** The app's origin. Debug builds also serve the UI from the dev server (`tauri android dev`),
   *  whose origins the build records (DEV_ORIGINS, see app/build.gradle.kts): never any origin. */
  private fun allowedOrigins(): Set<String> {
    val app = setOf("http://tauri.localhost", "https://tauri.localhost")
    if (!BuildConfig.DEBUG) return app
    return app + BuildConfig.DEV_ORIGINS.split(',').map { it.trim() }.filter { it.isNotEmpty() }
  }
}
