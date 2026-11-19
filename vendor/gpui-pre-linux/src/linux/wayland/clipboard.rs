use std::{
    fs::File,
    io::{ErrorKind, Write},
    os::fd::{AsRawFd, BorrowedFd, OwnedFd},
    time::Instant,
};

use calloop::{LoopHandle, PostAction};
use filedescriptor::{FileDescriptor, Pipe};
use strum::IntoEnumIterator;
use wayland_client::{Connection, protocol::wl_data_offer::WlDataOffer};
use wayland_protocols::wp::primary_selection::zv1::client::zwp_primary_selection_offer_v1::ZwpPrimarySelectionOfferV1;

use crate::linux::platform::{
    CLIPBOARD_READ_DEADLINE, PIPE_READ_MAX_BYTES, PIPE_READ_TIMEOUT, read_fd_async,
    read_fd_with_timeout,
};
use gpui::{BackgroundExecutor, ClipboardEntry, ClipboardItem, Image, ImageFormat, Task, hash};

/// Text mime types that we'll offer to other programs.
pub(crate) const TEXT_MIME_TYPES: [&str; 3] =
    ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain"];
pub(crate) const FILE_LIST_MIME_TYPE: &str = "text/uri-list";

/// Text mime types that we'll accept from other programs.
pub(crate) const ALLOWED_TEXT_MIME_TYPES: [&str; 2] = ["text/plain;charset=utf-8", "UTF8_STRING"];

pub(crate) struct Clipboard {
    connection: Connection,
    loop_handle: LoopHandle<'static, ()>,
    self_mime: String,

    // Internal clipboard
    contents: Option<ClipboardItem>,
    primary_contents: Option<ClipboardItem>,

    // External clipboard
    cached_read: Option<ClipboardItem>,
    current_offer: Option<DataOffer<WlDataOffer>>,
    cached_primary_read: Option<ClipboardItem>,
    current_primary_offer: Option<DataOffer<ZwpPrimarySelectionOfferV1>>,
}

pub(crate) trait ReceiveData {
    fn receive_data(&self, mime_type: String, fd: BorrowedFd<'_>);
}

impl ReceiveData for WlDataOffer {
    fn receive_data(&self, mime_type: String, fd: BorrowedFd<'_>) {
        self.receive(mime_type, fd);
    }
}

impl ReceiveData for ZwpPrimarySelectionOfferV1 {
    fn receive_data(&self, mime_type: String, fd: BorrowedFd<'_>) {
        self.receive(mime_type, fd);
    }
}

#[derive(Clone, Debug)]
/// Wrapper for `WlDataOffer` and `ZwpPrimarySelectionOfferV1`, used to help track mime types.
pub(crate) struct DataOffer<T: ReceiveData> {
    pub inner: T,
    mime_types: Vec<String>,
}

impl<T: ReceiveData> DataOffer<T> {
    pub fn new(offer: T) -> Self {
        Self {
            inner: offer,
            mime_types: Vec::new(),
        }
    }

    pub fn add_mime_type(&mut self, mime_type: String) {
        self.mime_types.push(mime_type)
    }

    fn has_mime_type(&self, mime_type: &str) -> bool {
        self.mime_types.iter().any(|t| t == mime_type)
    }

    /// Asks the selection owner to write `mime_type` and returns the read end.
    fn request(&self, connection: &Connection, mime_type: &str) -> FileDescriptor {
        let pipe = Pipe::new().unwrap();
        self.inner.receive_data(mime_type.to_string(), unsafe {
            BorrowedFd::borrow_raw(pipe.write.as_raw_fd())
        });
        let fd = pipe.read;
        drop(pipe.write);

        connection.flush().unwrap();
        fd
    }

    /// Reads `mime_type` without blocking a worker. `budget` is what is left of
    /// the limit this paste may read in total.
    async fn read_bytes_async(
        &self,
        connection: &Connection,
        executor: &BackgroundExecutor,
        mime_type: &str,
        deadline: Instant,
        budget: &mut usize,
    ) -> Option<Vec<u8>> {
        let fd = self.request(connection, mime_type);
        match read_fd_async(fd, executor, deadline, budget).await {
            Ok(bytes) => Some(bytes),
            Err(err) => {
                log::error!("error reading clipboard pipe: {err:?}");
                None
            }
        }
    }

    async fn read_async(
        &self,
        connection: &Connection,
        executor: &BackgroundExecutor,
    ) -> Option<ClipboardItem> {
        let deadline = Instant::now() + CLIPBOARD_READ_DEADLINE;
        let mut budget = PIPE_READ_MAX_BYTES;
        if let Some(mime_type) = self.text_mime_type() {
            let bytes = self
                .read_bytes_async(connection, executor, mime_type, deadline, &mut budget)
                .await;
            if let Some(item) = bytes.and_then(text_item) {
                return Some(item);
            }
        }
        for format in ImageFormat::iter() {
            let mime_type = format.mime_type();
            if !self.has_mime_type(mime_type) {
                continue;
            }
            if let Some(bytes) = self
                .read_bytes_async(connection, executor, mime_type, deadline, &mut budget)
                .await
            {
                return Some(image_item(format, bytes));
            }
        }
        None
    }

    fn text_mime_type(&self) -> Option<&str> {
        self.mime_types
            .iter()
            .find(|&mime_type| {
                ALLOWED_TEXT_MIME_TYPES
                    .iter()
                    .any(|&allowed| allowed == mime_type)
            })
            .map(String::as_str)
    }

    fn read_bytes(&self, connection: &Connection, mime_type: &str) -> Option<Vec<u8>> {
        let fd = self.request(connection, mime_type);
        match read_fd_with_timeout(fd, PIPE_READ_TIMEOUT) {
            Ok(bytes) => Some(bytes),
            Err(err) => {
                log::error!("error reading clipboard pipe: {err:?}");
                None
            }
        }
    }

    fn read_text(&self, connection: &Connection) -> Option<ClipboardItem> {
        let mime_type = self.text_mime_type()?;
        text_item(self.read_bytes(connection, mime_type)?)
    }

    fn read_image(&self, connection: &Connection) -> Option<ClipboardItem> {
        for format in ImageFormat::iter() {
            let mime_type = format.mime_type();
            if !self.has_mime_type(mime_type) {
                continue;
            }

            if let Some(bytes) = self.read_bytes(connection, mime_type) {
                return Some(image_item(format, bytes));
            }
        }
        None
    }
}

fn text_item(bytes: Vec<u8>) -> Option<ClipboardItem> {
    let text_content = match String::from_utf8(bytes) {
        Ok(content) => content,
        Err(e) => {
            log::error!("Failed to convert clipboard content to UTF-8: {}", e);
            return None;
        }
    };

    // Normalize the text to unix line endings, otherwise
    // copying from eg: firefox inserts a lot of blank
    // lines, and that is super annoying.
    Some(ClipboardItem::new_string(text_content.replace("\r\n", "\n")))
}

fn image_item(format: ImageFormat, bytes: Vec<u8>) -> ClipboardItem {
    let id = hash(&bytes);
    ClipboardItem {
        entries: vec![ClipboardEntry::Image(Image { format, bytes, id })],
    }
}

impl Clipboard {
    pub fn new(connection: Connection, loop_handle: LoopHandle<'static, ()>) -> Self {
        Self {
            connection,
            loop_handle,
            self_mime: format!("pid/{}", std::process::id()),

            contents: None,
            primary_contents: None,

            cached_read: None,
            current_offer: None,
            cached_primary_read: None,
            current_primary_offer: None,
        }
    }

    pub fn set(&mut self, item: ClipboardItem) {
        self.contents = Some(item);
    }

    pub fn set_primary(&mut self, item: ClipboardItem) {
        self.primary_contents = Some(item);
    }

    pub fn set_offer(&mut self, data_offer: Option<DataOffer<WlDataOffer>>) {
        self.cached_read = None;
        self.current_offer = data_offer;
    }

    pub fn set_primary_offer(&mut self, data_offer: Option<DataOffer<ZwpPrimarySelectionOfferV1>>) {
        self.cached_primary_read = None;
        self.current_primary_offer = data_offer;
    }

    pub fn self_mime(&self) -> String {
        self.self_mime.clone()
    }

    pub fn send(&self, _mime_type: String, fd: OwnedFd) {
        if let Some(text) = self.contents.as_ref().and_then(|contents| contents.text()) {
            self.send_bytes(fd, text.as_bytes().to_owned());
        }
    }

    pub fn send_primary(&self, _mime_type: String, fd: OwnedFd) {
        if let Some(text) = self
            .primary_contents
            .as_ref()
            .and_then(|contents| contents.text())
        {
            self.send_bytes(fd, text.as_bytes().to_owned());
        }
    }

    pub fn read(&mut self) -> Option<ClipboardItem> {
        let offer = self.current_offer.as_ref()?;
        if let Some(cached) = self.cached_read.clone() {
            return Some(cached);
        }

        if offer.has_mime_type(&self.self_mime) {
            return self.contents.clone();
        }

        let item = offer
            .read_text(&self.connection)
            .or_else(|| offer.read_image(&self.connection))?;

        self.cached_read = Some(item.clone());
        Some(item)
    }

    /// Like [`Self::read`], but the pipe is read on the background executor so
    /// a slow selection owner cannot stall the UI thread. The read yields the
    /// worker while it waits, ends at a 5 second deadline and takes at most
    /// 32 MiB over all formats. Dropping the task cancels it and closes the pipe.
    pub fn read_async(&mut self, executor: &BackgroundExecutor) -> Task<Option<ClipboardItem>> {
        let Some(offer) = self.current_offer.clone() else {
            return Task::ready(None);
        };
        if let Some(cached) = self.cached_read.clone() {
            return Task::ready(Some(cached));
        }
        if offer.has_mime_type(&self.self_mime) {
            return Task::ready(self.contents.clone());
        }

        let connection = self.connection.clone();
        let task_executor = executor.clone();
        executor.spawn(async move { offer.read_async(&connection, &task_executor).await })
    }

    pub fn read_primary(&mut self) -> Option<ClipboardItem> {
        let offer = self.current_primary_offer.as_ref()?;
        if let Some(cached) = self.cached_primary_read.clone() {
            return Some(cached);
        }

        if offer.has_mime_type(&self.self_mime) {
            return self.primary_contents.clone();
        }

        let item = offer
            .read_text(&self.connection)
            .or_else(|| offer.read_image(&self.connection))?;

        self.cached_primary_read = Some(item.clone());
        Some(item)
    }

    pub fn send_bytes(&self, fd: OwnedFd, bytes: Vec<u8>) {
        let mut written = 0;
        self.loop_handle
            .insert_source(
                calloop::generic::Generic::new(
                    File::from(fd),
                    calloop::Interest::WRITE,
                    calloop::Mode::Level,
                ),
                move |_, file, _| {
                    let file = unsafe { file.get_mut() };
                    loop {
                        match file.write(&bytes[written..]) {
                            Ok(n) if written + n == bytes.len() => {
                                written += n;
                                break Ok(PostAction::Remove);
                            }
                            Ok(n) => written += n,
                            Err(err) if err.kind() == ErrorKind::WouldBlock => {
                                break Ok(PostAction::Continue);
                            }
                            Err(_) => break Ok(PostAction::Remove),
                        }
                    }
                },
            )
            .unwrap();
    }
}
