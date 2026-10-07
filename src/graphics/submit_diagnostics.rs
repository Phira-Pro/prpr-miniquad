//! Optional managed-API submission diagnostics. No GL queries or GPU waits.
//! Counts API boundaries, not physical tile passes or executed GPU primitives.
use std::cell::{Cell, RefCell};

pub const TRACE_LIMIT: usize = 128;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SubmissionStats {
    pub enabled: bool,
    pub trace_enabled: bool,
    pub native_draw_calls: u64,
    pub instanced_api_calls: u64,
    pub multi_instance_calls: u64,
    pub zero_work_calls: u64,
    pub invalid_shape_calls: u64,
    pub rejected_draw_requests: u64,
    /// Requested positive index count × instances, not processed GPU work.
    pub nominal_index_instances: u64,
    pub pass_begin_api_calls: u64,
    pub pass_end_api_calls: u64,
    pub framebuffer_bind_calls: u64,
    pub pipeline_apply_calls: u64,
    pub binding_apply_calls: u64,
    pub trace_dropped_calls: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DrawSubmission {
    pub pipeline: usize,
    pub program: u32,
    /// Managed texture cache, not a query of raw external GL state.
    pub cached_texture0: u32,
    pub base_element: i32,
    pub elements: i32,
    pub instances: i32,
    pub instanced_api: bool,
}

#[derive(Default)]
pub(super) struct Counter {
    enabled: Cell<bool>,
    trace_enabled: Cell<bool>,
    stats: Cell<SubmissionStats>,
    trace: RefCell<Vec<DrawSubmission>>,
}
impl Counter {
    pub fn configure(&self, enabled: bool, trace: bool) {
        self.enabled.set(enabled);
        let allocated = enabled && trace && {
            let mut data = self.trace.borrow_mut();
            data.clear();
            data.try_reserve(TRACE_LIMIT).is_ok()
        };
        self.trace_enabled.set(allocated);
        self.reset();
    }
    pub fn enabled(&self) -> bool {
        self.enabled.get()
    }
    pub fn reset(&self) {
        self.stats.set(SubmissionStats::default());
        self.trace.borrow_mut().clear();
    }
    pub fn stats(&self) -> SubmissionStats {
        SubmissionStats {
            enabled: self.enabled.get(),
            trace_enabled: self.trace_enabled.get(),
            ..self.stats.get()
        }
    }
    pub fn trace(&self) -> Vec<DrawSubmission> {
        if self.trace_enabled.get() {
            self.trace.borrow().clone()
        } else {
            Vec::new()
        }
    }
    #[inline]
    fn update(&self, f: impl FnOnce(&mut SubmissionStats)) {
        if !self.enabled.get() {
            return;
        }
        let mut s = self.stats.get();
        f(&mut s);
        self.stats.set(s);
    }
    pub fn begin(&self, framebuffer_bound: bool) {
        self.update(|s| {
            s.pass_begin_api_calls = s.pass_begin_api_calls.saturating_add(1);
            s.framebuffer_bind_calls = s
                .framebuffer_bind_calls
                .saturating_add(framebuffer_bound as u64);
        });
    }
    pub fn end(&self) {
        self.update(|s| s.pass_end_api_calls = s.pass_end_api_calls.saturating_add(1));
    }
    pub fn framebuffer_bind(&self) {
        self.update(|s| s.framebuffer_bind_calls = s.framebuffer_bind_calls.saturating_add(1));
    }
    pub fn pipeline(&self) {
        self.update(|s| s.pipeline_apply_calls = s.pipeline_apply_calls.saturating_add(1));
    }
    pub fn bindings(&self) {
        self.update(|s| s.binding_apply_calls = s.binding_apply_calls.saturating_add(1));
    }
    pub fn rejected(&self) {
        self.update(|s| s.rejected_draw_requests = s.rejected_draw_requests.saturating_add(1));
    }
    pub fn draw(&self, draw: DrawSubmission) {
        self.update(|s| {
            s.native_draw_calls = s.native_draw_calls.saturating_add(1);
            s.instanced_api_calls = s
                .instanced_api_calls
                .saturating_add(draw.instanced_api as u64);
            s.multi_instance_calls = s
                .multi_instance_calls
                .saturating_add((draw.instances > 1) as u64);
            s.zero_work_calls = s
                .zero_work_calls
                .saturating_add((draw.instances == 0 || draw.elements == 0) as u64);
            s.invalid_shape_calls = s
                .invalid_shape_calls
                .saturating_add((draw.instances < 0 || draw.elements < 0) as u64);
            if draw.instances > 0 && draw.elements > 0 {
                s.nominal_index_instances = s
                    .nominal_index_instances
                    .saturating_add(draw.instances as u64 * draw.elements as u64);
            }
        });
        if self.trace_enabled.get() {
            let mut data = self.trace.borrow_mut();
            if data.len() < TRACE_LIMIT {
                data.push(draw);
            } else {
                self.update(|s| s.trace_dropped_calls = s.trace_dropped_calls.saturating_add(1));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn draw(i: i32, elements: i32, instances: i32, instanced_api: bool) -> DrawSubmission {
        DrawSubmission {
            pipeline: i as usize,
            program: i as u32,
            cached_texture0: 17,
            base_element: i,
            elements,
            instances,
            instanced_api,
        }
    }
    #[test]
    fn disabled_configuration_and_reset_do_not_leak_previous_work() {
        let c = Counter::default();
        for _ in 0..3 {
            c.configure(true, true);
            c.draw(draw(1, 6, 1, true));
            c.begin(true);
            c.end();
            assert_eq!(c.stats().native_draw_calls, 1);
            c.configure(false, true);
            c.draw(draw(2, 99, 5, true));
            c.begin(true);
            c.end();
            c.rejected();
            assert_eq!(c.stats(), SubmissionStats::default());
            assert!(c.trace().is_empty());
        }
    }
    #[test]
    fn actual_api_kind_zero_work_and_rejected_requests_are_distinct() {
        let c = Counter::default();
        c.configure(true, true);
        c.draw(draw(0, 6, 1, true));
        c.draw(draw(1, 6, 1, false));
        c.draw(draw(2, 6, 0, true));
        c.draw(draw(3, 0, 5, true));
        c.draw(draw(4, -1, 1, true));
        c.rejected();
        let s = c.stats();
        assert_eq!(
            (
                s.native_draw_calls,
                s.instanced_api_calls,
                s.multi_instance_calls,
                s.zero_work_calls,
                s.invalid_shape_calls,
                s.rejected_draw_requests
            ),
            (5, 4, 1, 2, 1, 1)
        );
        assert_eq!(s.nominal_index_instances, 12);
        assert_eq!(c.trace().len(), 5);
    }
    #[test]
    fn bounded_ordered_trace_and_totals_match_an_independent_command_log() {
        let c = Counter::default();
        c.configure(true, true);
        let commands: Vec<_> = (0..1000)
            .map(|i| draw(i, 6 + (i % 5) * 3, i % 4, i % 3 != 0))
            .collect();
        for command in &commands {
            c.draw(*command);
        }
        assert_eq!(c.trace(), commands[..TRACE_LIMIT]);
        let s = c.stats();
        assert_eq!(s.trace_dropped_calls, (commands.len() - TRACE_LIMIT) as u64);
        assert_eq!(s.native_draw_calls, commands.len() as u64);
        assert_eq!(
            s.nominal_index_instances,
            commands
                .iter()
                .map(|d| d.elements as u64 * d.instances as u64)
                .sum::<u64>()
        );
        let capacity = c.trace.borrow().capacity();
        c.reset();
        c.draw(commands[999]);
        assert_eq!(c.trace(), vec![commands[999]]);
        assert_eq!(c.trace.borrow().capacity(), capacity);
    }
    #[test]
    fn nominal_work_saturates_instead_of_wrapping() {
        let c = Counter::default();
        c.configure(true, false);
        for i in 0..5 {
            c.draw(draw(i, i32::MAX, i32::MAX, true));
        }
        assert_eq!(c.stats().nominal_index_instances, u64::MAX);
        assert!(c.trace().is_empty());
    }
}
