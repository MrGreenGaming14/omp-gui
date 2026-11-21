//! The session screen's components over the session DOM: the virtual [`transcript`], the
//! [`composer`] and its pickers, and the [`decisions`] cards with their tracker.

pub mod composer;
pub mod decisions;
pub mod transcript;

#[cfg(test)]
mod tests;
