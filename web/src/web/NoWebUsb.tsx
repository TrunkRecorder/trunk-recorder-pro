/** True when this browser can talk to a USB dongle. WebUSB only exists on
 *  Chromium browsers, and only on secure (https or localhost) pages. */
export function hasWebUsb(): boolean {
  return "usb" in navigator;
}

/** Shown in place of the app when WebUSB is missing. */
export function NoWebUsb() {
  // Chromium hides navigator.usb on plain-http pages, so a supported browser
  // can still land here; tell that case apart from an unsupported browser.
  const insecure = !window.isSecureContext;
  return (
    <div className="no-webusb">
      <div className="panel">
        <div className="brand">
          <div className="logo" />
          <h1>Trunk Recorder Pro</h1>
        </div>
        {insecure ? (
          <>
            <h2>This page needs a secure connection</h2>
            <p>
              WebUSB, which reaches your SDR, only works on <code>https://</code> or <code>localhost</code> pages.
            </p>
          </>
        ) : (
          <>
            <h2>This browser can't open Trunk Recorder Pro</h2>
            <p>It needs WebUSB to reach your SDR. Use one of these:</p>
            <ul>
              <li><strong>Google Chrome</strong>, <strong>Microsoft Edge</strong>, <strong>Opera</strong> or <strong>Brave</strong> on Windows, macOS, Linux or ChromeOS</li>
              <li><strong>Chrome</strong> on Android, with the dongle on a USB-OTG adapter</li>
            </ul>
            <p className="muted">Safari, Firefox and iPhone / iPad browsers lack WebUSB; use the desktop app.</p>
          </>
        )}
      </div>
    </div>
  );
}
