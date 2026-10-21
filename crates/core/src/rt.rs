/// Runs blocking work from async code without parking the async worker: on a multi-thread
/// runtime the worker first hands its other tasks to another thread (`block_in_place`).
pub fn offload<T>(job: impl FnOnce() -> T) -> T {
    match tokio::runtime::Handle::try_current() {
        Ok(h) if h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => tokio::task::block_in_place(job),
        _ => job(),
    }
}
