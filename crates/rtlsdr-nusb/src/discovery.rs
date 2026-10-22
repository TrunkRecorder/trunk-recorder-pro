//! Device enumeration, selection, and USB identity completion.
use crate::maybe_future::{Either, MaybeFutureExt, NonWasmSend, ready};
use crate::{Error, Result};
use nusb::MaybeFuture;
use std::num::NonZeroU8;

/// USB identity collected during enumeration and completed during open.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceDescriptor {
    /// Index among currently visible devices with the selected USB IDs.
    pub index: usize,
    /// USB vendor ID.
    pub vid: u16,
    /// USB product ID.
    pub pid: u16,
    /// Device serial string, when the OS/browser supplies it.
    pub serial: Option<String>,
    /// Manufacturer string, when available.
    pub manufacturer: Option<String>,
    /// Product string, when available.
    pub product: Option<String>,
}
impl DeviceDescriptor {
    pub(crate) fn from_nusb(index: usize, info: &nusb::DeviceInfo) -> Self {
        Self {
            index,
            vid: info.vendor_id(),
            pid: info.product_id(),
            serial: info.serial_number().map(str::to_owned),
            manufacturer: info.manufacturer_string().map(str::to_owned),
            product: info.product_string().map(str::to_owned),
        }
    }
    pub(crate) fn blog_v4(&self) -> bool {
        self.manufacturer.as_deref() == Some("RTLSDRBlog")
            && self.product.as_deref() == Some("Blog V4")
    }
    pub(crate) fn read_missing_strings<F, M>(
        mut self,
        manufacturer_index: Option<NonZeroU8>,
        product_index: Option<NonZeroU8>,
        mut read: F,
    ) -> impl MaybeFuture<Output = Self>
    where
        F: FnMut(NonZeroU8) -> M + NonWasmSend,
        M: MaybeFuture<Output = Option<String>>,
    {
        ready(()).continue_with(move |()| {
            let manufacturer = match manufacturer_index.filter(|_| self.manufacturer.is_none()) {
                Some(index) => Either::left(read(index)),
                None => Either::right(ready(None)),
            };
            manufacturer.continue_with(move |value| {
                self.manufacturer = self.manufacturer.or(value);
                let product = match product_index.filter(|_| self.product.is_none()) {
                    Some(index) => Either::left(read(index)),
                    None => Either::right(ready(None)),
                };
                product.map(move |value| {
                    self.product = self.product.or(value);
                    self
                })
            })
        })
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Selector {
    pub index: Option<usize>,
    pub serial: Option<String>,
    pub usb_id: Option<(u16, u16)>,
}
impl Selector {
    pub(crate) fn matches_id(&self, vid: u16, pid: u16) -> bool {
        self.usb_id
            .map_or(vid == 0x0bda && matches!(pid, 0x2832 | 0x2838), |pair| {
                pair == (vid, pid)
            })
    }
    fn matches(&self, index: usize, info: &nusb::DeviceInfo) -> bool {
        self.index.is_none_or(|i| i == index)
            && self
                .serial
                .as_deref()
                .is_none_or(|s| info.serial_number() == Some(s))
    }
}
pub(crate) fn list() -> impl MaybeFuture<Output = Result<Vec<DeviceDescriptor>>> {
    nusb::list_devices().map(|devices| {
        Ok(devices?
            .filter(|d| Selector::default().matches_id(d.vendor_id(), d.product_id()))
            .enumerate()
            .map(|(i, d)| DeviceDescriptor::from_nusb(i, &d))
            .collect())
    })
}
pub(crate) fn select(
    selector: Selector,
) -> impl MaybeFuture<Output = Result<(DeviceDescriptor, nusb::DeviceInfo)>> {
    nusb::list_devices().map(move |devices| {
        devices?
            .filter(|d| selector.matches_id(d.vendor_id(), d.product_id()))
            .enumerate()
            .find(|(i, d)| selector.matches(*i, d))
            .map(|(i, d)| (DeviceDescriptor::from_nusb(i, &d), d))
            .ok_or(Error::DeviceNotFound)
    })
}
#[cfg(target_arch = "wasm32")]
pub(crate) async fn request_permission(selector: &Selector) -> Result<()> {
    let pairs = selector
        .usb_id
        .map_or_else(|| vec![(0x0bda, 0x2832), (0x0bda, 0x2838)], |p| vec![p]);
    let selectors: Vec<_> = pairs
        .into_iter()
        .map(|(vid, pid)| {
            let mut filter = nusb::DeviceSelector::all().with_vid_pid(vid, pid);
            if let Some(serial) = &selector.serial {
                filter = filter.with_serial_number(serial.clone());
            }
            filter
        })
        .collect();
    nusb::request_device(&selectors)
        .await?
        .ok_or(Error::DeviceNotFound)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        future::IntoFuture,
        sync::{Arc, Mutex},
    };

    fn descriptor(manufacturer: Option<&str>, product: Option<&str>) -> DeviceDescriptor {
        DeviceDescriptor {
            index: 0,
            vid: 0x0bda,
            pid: 0x2838,
            serial: Some("00000001".into()),
            manufacturer: manufacturer.map(str::to_owned),
            product: product.map(str::to_owned),
        }
    }

    #[test]
    fn missing_windows_manufacturer_is_read_lazily_and_enables_v4() {
        let reads = Arc::new(Mutex::new(Vec::new()));
        let observed = reads.clone();
        let operation = descriptor(None, Some("Blog V4")).read_missing_strings(
            NonZeroU8::new(1),
            NonZeroU8::new(2),
            move |index| {
                observed.lock().unwrap().push(index.get());
                ready(Some("RTLSDRBlog".into()))
            },
        );
        assert!(reads.lock().unwrap().is_empty());
        let result = operation.wait();
        assert!(result.blog_v4());
        assert_eq!(result.serial.as_deref(), Some("00000001"));
        assert_eq!(*reads.lock().unwrap(), [1]);
    }

    #[test]
    fn async_open_recovers_both_branding_strings() {
        let result = futures_lite::future::block_on(
            descriptor(None, None)
                .read_missing_strings(NonZeroU8::new(1), NonZeroU8::new(2), |index| {
                    ready(Some(
                        match index.get() {
                            1 => "RTLSDRBlog",
                            2 => "Blog V4",
                            _ => unreachable!(),
                        }
                        .into(),
                    ))
                })
                .into_future(),
        );
        assert!(result.blog_v4());
    }

    #[test]
    fn cached_absent_and_unreadable_strings_do_not_fabricate_v4_branding() {
        let cached = descriptor(Some("Other"), Some("Blog V4"));
        let mut reads = 0;
        let result = cached
            .clone()
            .read_missing_strings(NonZeroU8::new(1), NonZeroU8::new(2), |_| {
                reads += 1;
                ready(None)
            })
            .wait();
        assert_eq!(result, cached);
        assert_eq!(reads, 0);
        let result = descriptor(None, None)
            .read_missing_strings(None, NonZeroU8::new(2), |index| {
                assert_eq!(index.get(), 2);
                ready(None)
            })
            .wait();
        assert!(!result.blog_v4());
        assert_eq!(result.manufacturer, None);
        assert_eq!(result.product, None);
    }
}
