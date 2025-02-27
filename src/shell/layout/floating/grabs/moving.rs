// SPDX-License-Identifier: GPL-3.0-only

use crate::{
    backend::render::element::AsGlowRenderer,
    shell::{
        element::{CosmicMapped, CosmicMappedRenderElement},
        focus::target::{KeyboardFocusTarget, PointerFocusTarget},
    },
    utils::prelude::*,
};

use smithay::{
    backend::renderer::{
        element::{AsRenderElements, RenderElement},
        ImportAll, ImportMem, Renderer,
    },
    desktop::space::SpaceElement,
    input::{
        pointer::{
            AxisFrame, ButtonEvent, GrabStartData as PointerGrabStartData, MotionEvent,
            PointerGrab, PointerInnerHandle, RelativeMotionEvent,
        },
        Seat,
    },
    output::Output,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{IsAlive, Logical, Point, Rectangle, Serial},
    wayland::compositor::SurfaceData,
};
use std::{cell::RefCell, time::Duration};

pub type SeatMoveGrabState = RefCell<Option<MoveGrabState>>;

pub struct MoveGrabState {
    window: CosmicMapped,
    initial_cursor_location: Point<f64, Logical>,
    initial_window_location: Point<i32, Logical>,
}

impl MoveGrabState {
    pub fn render<I, R>(&self, renderer: &mut R, seat: &Seat<State>, output: &Output) -> Vec<I>
    where
        R: Renderer + ImportAll + ImportMem + AsGlowRenderer,
        <R as Renderer>::TextureId: 'static,
        CosmicMappedRenderElement<R>: RenderElement<R>,
        I: From<CosmicMappedRenderElement<R>>,
    {
        let cursor_at = seat.get_pointer().unwrap().current_location();
        let delta = cursor_at - self.initial_cursor_location;
        let location = self.initial_window_location.to_f64() + delta;

        let mut window_geo = self.window.geometry();
        window_geo.loc += location.to_i32_round();
        if !output.geometry().intersection(window_geo).is_some() {
            return Vec::new();
        }

        let scale = output.current_scale().fractional_scale().into();
        AsRenderElements::<R>::render_elements::<I>(
            &self.window,
            renderer,
            (location.to_i32_round() - output.geometry().loc - self.window.geometry().loc)
                .to_physical_precise_round(scale),
            scale,
        )
    }

    pub fn send_frames(
        &self,
        output: &Output,
        time: impl Into<Duration>,
        throttle: Option<Duration>,
        primary_scan_out_output: impl FnMut(&WlSurface, &SurfaceData) -> Option<Output> + Copy,
    ) {
        self.window
            .active_window()
            .send_frame(output, time, throttle, primary_scan_out_output)
    }
}

pub struct MoveSurfaceGrab {
    window: CosmicMapped,
    start_data: PointerGrabStartData<State>,
    seat: Seat<State>,
}

impl PointerGrab<State> for MoveSurfaceGrab {
    fn motion(
        &mut self,
        state: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        _focus: Option<(PointerFocusTarget, Point<i32, Logical>)>,
        event: &MotionEvent,
    ) {
        // While the grab is active, no client has pointer focus
        handle.motion(state, None, event);
        if !self.window.alive() {
            self.ungrab(state, handle, event.serial, event.time);
        }
    }

    fn relative_motion(
        &mut self,
        state: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        _focus: Option<(PointerFocusTarget, Point<i32, Logical>)>,
        event: &RelativeMotionEvent,
    ) {
        // While the grab is active, no client has pointer focus
        handle.relative_motion(state, None, event);
    }

    fn button(
        &mut self,
        state: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &ButtonEvent,
    ) {
        handle.button(state, event);
        if handle.current_pressed().is_empty() {
            self.ungrab(state, handle, event.serial, event.time);
        }
    }

    fn axis(
        &mut self,
        state: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        details: AxisFrame,
    ) {
        handle.axis(state, details);
    }

    fn start_data(&self) -> &PointerGrabStartData<State> {
        &self.start_data
    }
}

impl MoveSurfaceGrab {
    pub fn new(
        start_data: PointerGrabStartData<State>,
        window: CosmicMapped,
        seat: &Seat<State>,
        initial_cursor_location: Point<f64, Logical>,
        initial_window_location: Point<i32, Logical>,
    ) -> MoveSurfaceGrab {
        let grab_state = MoveGrabState {
            window: window.clone(),
            initial_cursor_location,
            initial_window_location,
        };

        *seat
            .user_data()
            .get::<SeatMoveGrabState>()
            .unwrap()
            .borrow_mut() = Some(grab_state);

        MoveSurfaceGrab {
            window,
            start_data,
            seat: seat.clone(),
        }
    }

    fn ungrab(
        &mut self,
        state: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        serial: Serial,
        time: u32,
    ) {
        // No more buttons are pressed, release the grab.
        let output = self.seat.active_output();

        if let Some(grab_state) = self
            .seat
            .user_data()
            .get::<SeatMoveGrabState>()
            .and_then(|s| s.borrow_mut().take())
        {
            if grab_state.window.alive() {
                let delta = handle.current_location() - grab_state.initial_cursor_location;
                let window_location = (grab_state.initial_window_location.to_f64() + delta)
                    .to_i32_round()
                    - output.geometry().loc;

                let workspace_handle = state.common.shell.active_space(&output).handle;
                for (window, _) in grab_state.window.windows() {
                    state
                        .common
                        .shell
                        .toplevel_info_state
                        .toplevel_enter_workspace(&window, &workspace_handle);
                    state
                        .common
                        .shell
                        .toplevel_info_state
                        .toplevel_enter_output(&window, &output);
                }

                let offset = state
                    .common
                    .shell
                    .active_space(&output)
                    .floating_layer
                    .space
                    .output_geometry(&output)
                    .unwrap()
                    .loc;
                grab_state.window.set_geometry(Rectangle::from_loc_and_size(
                    window_location + offset,
                    grab_state.window.geometry().size,
                ));
                state
                    .common
                    .shell
                    .active_space_mut(&output)
                    .floating_layer
                    .map_internal(grab_state.window, &output, Some(window_location + offset));
            }
        }

        handle.unset_grab(state, serial, time);
        if self.window.alive() {
            Common::set_focus(
                state,
                Some(&KeyboardFocusTarget::from(self.window.clone())),
                &self.seat,
                Some(serial),
            )
        }
    }
}
