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
              Trunk Recorder Pro reaches your SDR dongle through WebUSB, and browsers only allow WebUSB on pages
              served over <code>https://</code> (or from <code>localhost</code>). This page was loaded over plain{" "}
              <code>http://</code>.
            </p>
            <p>Open it again from an https address, or from localhost on this computer.</p>
          </>
        ) : (
          <>
            <h2>This browser can't open Trunk Recorder Pro</h2>
            <p>
              The browser version talks to your SDR dongle directly over USB, using a feature called WebUSB. This
              browser doesn't have it, so there's no way for the page to reach the radio.
            </p>
            <p>Open this page in one of these instead:</p>
            <ul>
              <li><strong>Google Chrome</strong>, <strong>Microsoft Edge</strong>, <strong>Opera</strong> or <strong>Brave</strong> on Windows, macOS, Linux or ChromeOS</li>
              <li><strong>Chrome</strong> on Android, with the dongle on a USB-OTG adapter</li>
            </ul>
            <p className="muted">
              Safari, Firefox, and every browser on iPhone and iPad don't support WebUSB. On those, use the desktop
              version of Trunk Recorder Pro.
            </p>
          </>
        )}
      </div>
    </div>
  );
}
