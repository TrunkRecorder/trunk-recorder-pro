import type { GainConfig, SignalSource } from "./SignalSource.ts";
import { RTLSDRGains } from "./RTLSDRGains.ts";

// Real RTL-SDR dongle over WebUSB. Adapts the `@jtarrio/webrtlsdr` library
// (the plan's primary dependency) to our SignalSource contract — the only piece
// that fundamentally changes vs. native RTLSDRSource.swift.
//
// Threading (plan §5): the device is opened and read INSIDE the worker so the
// whole radio path is off the main thread. webrtlsdr's RTL2832U_Provider.get()
// calls navigator.usb.requestDevice() internally, which requires a user gesture
// and is Window-only — it does NOT work in a worker. So we split it:
//   • The MAIN thread grants the device once, from the Start click, via
//     navigator.usb.requestDevice(RTL_USB_FILTERS) (see appStore.start()).
//   • The WORKER re-acquires that already-granted device with
//     navigator.usb.getDevices() (worker-safe) and opens it with the low-level
//     RTL2832U.open(device) — bypassing the provider's requestDevice.
//
// Maps native librtlsdr calls -> webrtlsdr:
//   rtlsdr_open              -> RTL2832U.open(device)  (device from getDevices)
//   rtlsdr_set_sample_rate   -> device.setSampleRate(hz)
//   rtlsdr_set_center_freq   -> device.setCenterFrequency(hz)  (dominant cost)
//   rtlsdr_set_tuner_gain*   -> device.setGain(dbOrNull)  (null = auto/AGC)
//   rtlsdr_set_freq_correction -> device.setFrequencyCorrection(ppm)
//   rtlsdr_reset_buffer      -> device.resetBuffer()
//   rtlsdr_read_sync(len)    -> device.readSamples(count) -> {data: ArrayBuffer}
//   rtlsdr_close             -> device.close()

/** Minimal shape of the webrtlsdr device we use. */
interface RtlDevice {
  setSampleRate(rate: number): Promise<number>;
  setCenterFrequency(freq: number): Promise<number>;
  setGain(gain: number | null): Promise<void>;
  setFrequencyCorrection?(ppm: number): Promise<void>;
  resetBuffer(): Promise<void>;
  readSamples(length: number): Promise<{ data: ArrayBuffer }>;
  close(): Promise<void>;
}

/** RTL2832U USB vendor/product IDs (matches webrtlsdr's TUNERS). Used for the
 *  main-thread requestDevice grant and the worker-side getDevices() match. */
export const RTL_USB_FILTERS = [
  { vendorId: 0x0bda, productId: 0x2832 },
  { vendorId: 0x0bda, productId: 0x2838 },
];

function matchesRtl(d: USBDevice): boolean {
  return RTL_USB_FILTERS.some((f) => d.vendorId === f.vendorId && d.productId === f.productId);
}

/** A granted RTL-SDR, in the shape the settings UI shows (serialisable — a raw
 *  USBDevice can't cross a postMessage boundary, and the store never needs to). */
export interface SdrDeviceInfo {
  /** USB serial-number string descriptor, or "" when the dongle reports none.
   *  This is the only stable identifier we can pass to the worker to pick one
   *  specific device among several, so an empty serial means "not selectable". */
  serial: string;
  product: string;
  manufacturer: string;
  vendorId: number;
  productId: number;
  opened: boolean;
}

function describe(d: USBDevice): SdrDeviceInfo {
  return {
    serial: d.serialNumber ?? "",
    product: d.productName ?? "RTL-SDR",
    manufacturer: d.manufacturerName ?? "",
    vendorId: d.vendorId,
    productId: d.productId,
    opened: d.opened,
  };
}

export class WebRtlSource implements SignalSource {
  readonly name = "RTL-SDR";
  readonly kind = "usb" as const;
  // Four transfers in flight (~128 ms of air at 2 MSPS with the engine's 32 ms
  // chunks): the on-chip FIFO is only milliseconds deep, so this queue is what
  // rides out a stall on the engine thread. webrtlsdr's readSamples is a bare
  // transferIn with no lock, and @jtarrio/signals' own Radio streams exactly
  // this way (several readSamples loops at once).
  readonly readAheadDepth = 4;
  private device: RtlDevice | null = null;

  /** @param preferredSerial pick the granted device whose USB serial matches, so
   *  the user can choose between several dongles. Falls back to the first match
   *  when omitted or when no granted device carries that serial. */
  private readonly preferredSerial?: string;

  constructor(preferredSerial?: string) {
    this.preferredSerial = preferredSerial;
  }

  static isSupported(): boolean {
    return typeof navigator !== "undefined" && "usb" in navigator;
  }

  /** Granted RTL-SDR dongles, worker-safe (getDevices needs no user gesture). */
  static async listGranted(): Promise<SdrDeviceInfo[]> {
    if (!WebRtlSource.isSupported()) return [];
    try {
      const devices = await navigator.usb.getDevices();
      return devices.filter(matchesRtl).map(describe);
    } catch {
      return [];
    }
  }

  async open(): Promise<void> {
    if (!WebRtlSource.isSupported()) {
      throw new Error(
        "WebUSB is not available. Use Chrome, Edge, or another Chromium browser over HTTPS or localhost."
      );
    }
    // The package's exports map has no `types` condition, so tsc can't resolve
    // the subpath's declarations even though Vite/Rollup bundle it fine. We only
    // need the runtime value here, so ignore the type-resolution error.
    // @ts-ignore — optional dependency subpath, resolved by the bundler.
    const mod = (await import("@jtarrio/webrtlsdr/rtlsdr.js")) as unknown as {
      RTL2832U: { open(device: USBDevice): Promise<RtlDevice> };
    };
    const devices = (await navigator.usb.getDevices()).filter(matchesRtl);
    // Prefer the device the user picked in Settings (by serial); fall back to the
    // first granted dongle when that one is gone or reports no serial.
    const usbDevice =
      (this.preferredSerial && devices.find((d) => d.serialNumber === this.preferredSerial)) || devices[0];
    if (!usbDevice) {
      throw new Error(
        "No granted RTL-SDR device. Click Start to pick your dongle, and check the OS driver " +
          "(Windows: WinUSB via Zadig; Linux: unbind dvb_usb_rtl28xxu)."
      );
    }
    // A prior aborted Start (or another tab) can leave this same USBDevice opened
    // but unclaimed within the process. Release it first so we start from a known
    // closed state and re-open cleanly below.
    if (usbDevice.opened) {
      try {
        await usbDevice.close();
      } catch {
        /* best-effort */
      }
    }
    try {
      // RTL2832U.open() goes straight to claimInterface() — it expects an
      // already-open device. The library's own Provider calls device.open()
      // first (RTL2832U_Provider.get); we bypass the Provider (its requestDevice
      // is Window-only, see above), so WE must open + configure the device here.
      // Skipping this makes claimInterface throw InvalidStateError, which the
      // library rewrites into a misleading "in use by another application" error.
      await usbDevice.open();
      if (usbDevice.configuration === null) {
        await usbDevice.selectConfiguration(1);
      }
      this.device = await mod.RTL2832U.open(usbDevice);
    } catch (e) {
      // Opening does device.open() then claimInterface(); if the claim fails the
      // device is left open. Release it so the next attempt starts clean.
      try {
        await usbDevice.close();
      } catch {
        /* best-effort */
      }
      this.device = null;
      const cause = e instanceof Error ? e.message : String(e);
      // Only the interface-claim failure means "another app has the device";
      // other failures (e.g. a transfer error while re-initialising the tuner)
      // are usually a flaky USB link or a too-fast re-open, so don't misdirect.
      const busy = /another application|claim|access|SecurityError|InvalidState/i.test(cause);
      const hint = busy
        ? " On macOS, quit apps that grab USB HID devices (e.g. Elgato Stream Deck / Control " +
          "Center), then unplug/replug the dongle. If it persists, reload this tab."
        : " This is usually a flaky USB link — try a direct port (not a hub/dock), a 2.56 MSPS " +
          "sample rate, or unplug/replug the dongle. If it persists, reload this tab.";
      throw new Error(cause + hint);
    }
  }

  async setSampleRate(hz: number): Promise<void> {
    await this.dev().setSampleRate(hz);
  }

  async setCenterFreq(hz: number): Promise<void> {
    await this.dev().setCenterFrequency(hz);
  }

  async setGain(config: GainConfig): Promise<void> {
    // webrtlsdr: null = automatic gain; a number = manual gain in dB.
    if (config.kind === "auto") {
      await this.dev().setGain(null);
    } else {
      await this.dev().setGain(config.tenthsDb / 10);
    }
  }

  async setFreqCorrection(ppm: number): Promise<void> {
    const d = this.dev();
    if (d.setFrequencyCorrection) await d.setFrequencyCorrection(ppm);
  }

  async prepareRead(): Promise<void> {
    await this.dev().resetBuffer();
  }

  async readIQ(sampleCount: number): Promise<Uint8Array> {
    // webrtlsdr returns { data: ArrayBuffer } of interleaved (U8 I, U8 Q) pairs.
    const block = await this.dev().readSamples(sampleCount);
    return new Uint8Array(block.data);
  }

  async close(): Promise<void> {
    if (this.device) {
      try {
        await this.device.close();
      } catch {
        /* ignore — releasing the USB claim best-effort */
      }
      this.device = null;
    }
  }

  /** webrtlsdr does not enumerate tuner gain steps; use the standard R820T2
   *  table so the manual-gain slider can snap to plausible steps. */
  availableGainsDb(): number[] {
    return RTLSDRGains.standard;
  }

  private dev(): RtlDevice {
    if (!this.device) throw new Error("RTL-SDR device not open.");
    return this.device;
  }
}
