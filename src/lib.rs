mod crash;

cfg_if::cfg_if! {
    if #[cfg(feature = "semaphore")] {
        mod semaphore;
        pub use semaphore::{Semaphore, SemaphorePermission, SemaphoreSettings};
    }
}

pub use crash::CortexError;

pub type CortexResult<T> = std::result::Result<T, CortexError>;

/// Attempt to clean up a segment of shared memory
fn try_clear_mem(id: i32) -> CortexResult<()> {
    unsafe {
        if libc::shmctl(id, libc::IPC_RMID, std::ptr::null_mut()) == -1 {
            return Err(CortexError::new_dirty(format!(
                "Error cleaning up shared memory with id: {}",
                id
            )));
        }
    }
    Ok(())
}

pub trait CortexSync: Sized {
    type Settings;

    fn new(cortex_key: i32, settings: Option<&Self::Settings>) -> CortexResult<Self>;
    fn attach(cortex_key: i32) -> CortexResult<Self>;
    fn read_lock(&self) -> CortexResult<()>;
    fn write_lock(&self) -> CortexResult<()>;
    fn release(&self) -> CortexResult<()>;
}

#[derive(Debug)]
pub struct Cortex<T, L> {
    key: i32,
    id: i32,
    #[allow(dead_code)]
    size: usize,
    is_owner: bool,
    lock: L,
    ptr: *mut T,
}

// TODO: builder that takes only a data of typ T
pub struct CortexBuilder<T, L> {
    data: T,
    key: i32,
    lock: Option<L>,
}

impl<T, L: CortexSync> CortexBuilder<T, L> {
    pub fn new(data: T) -> Self {
        Self {
            data,
            key: 1, // todo: random
            lock: None,
        }
    }
    pub fn key(mut self, key: i32) -> Self {
        self.key = key;
        self
    }
    pub fn lock(mut self, lock_settings: L::Settings) -> Self {
        let lock = L::new(self.key, Some(&lock_settings)).unwrap();
        self.lock.replace(lock);
        self
    }
}

impl<T, L: CortexSync> Cortex<T, L> {
    /// Allocate a new segment of shared memory
    pub fn new(key: i32, data: T, lock_settings: Option<&L::Settings>) -> CortexResult<Self> {
        let lock = L::new(key, lock_settings)?;

        // Allocate memory
        let size = std::mem::size_of::<T>();
        let permissions = libc::IPC_CREAT | libc::IPC_EXCL | 0o666;
        let id = unsafe { libc::shmget(key, size, permissions) };
        if id == -1 {
            try_clear_mem(id)?
        } else {
            tracing::trace!("Allocated {} bytes with id {}", size, id);
        }

        // Attach memory to current process and get a pointer
        let ptr = unsafe { libc::shmat(id, std::ptr::null_mut(), 0) as *mut T };
        if ptr as isize == -1 {
            try_clear_mem(id)?;
        } else {
            tracing::trace!("Successfully attached shared memory");
        }

        unsafe {
            ptr.write(data);
        }

        Ok(Self {
            id,
            key,
            size,
            is_owner: true,
            lock,
            ptr,
        })
    }
    /// Attempt to attach to an already existing segment of shared memory
    pub fn attach(key: i32) -> CortexResult<Self> {
        let lock = L::attach(key)?;

        let id = unsafe {
            libc::shmget(key, 0, 0o666) // Size is 0 since we're not creating the segment
        };
        if id == -1 {
            return Err(CortexError::new_clean(format!(
                "Error during shmget for key {}",
                key,
            )));
        } else {
            tracing::trace!("Found shared memory with id {}", id);
        }

        let ptr = unsafe { libc::shmat(id, std::ptr::null_mut(), 0) as *mut T };
        if ptr as isize == -1 {
            return Err(CortexError::new_clean("Error during shmat"));
        } else {
            tracing::trace!("Successfully attached shared memory");
        }

        Ok(Self {
            id,
            key,
            size: std::mem::size_of::<T>(),
            is_owner: false,
            lock,
            ptr,
        })
    }
    /// Read from shared memory
    pub fn read(&self) -> CortexResult<T> {
        unsafe {
            self.lock.read_lock()?;
            let data = self.ptr.read();
            self.lock.release()?;
            Ok(data)
        }
    }
    /// Write to shared memory
    pub fn write(&self, data: T) -> CortexResult<()> {
        unsafe {
            self.lock.write_lock()?;
            self.ptr.write(data);
            self.lock.release()?;
        }
        Ok(())
    }
    pub fn key(&self) -> i32 {
        self.key
    }
}

/// Drop a segment of shared memory
impl<T, L> Drop for Cortex<T, L> {
    fn drop(&mut self) {
        if !self.is_owner {
            return;
        }
        if let Err(err) = try_clear_mem(self.id) {
            tracing::error!("Error during Drop: {}", err)
        }
    }
}
