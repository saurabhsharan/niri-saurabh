use niri_config::{
    Color, CornerRadius, FloatOrInt, GradientInterpolation, Shadow as ShadowConfig, ShadowOffset,
};
use smithay::backend::renderer::element::utils::{
    Relocate, RelocateRenderElement, RescaleRenderElement,
};
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::GlesTexProgram;
use smithay::backend::renderer::Color32F;
use smithay::output::Output;
use smithay::utils::{Logical, Point, Rectangle, Scale, Size};

use crate::layout::shadow::Shadow as LayoutShadow;
use crate::layout::{LayoutElement as _, LayoutElementRenderElement};
use crate::niri::Niri;
use crate::niri_render_elements;
use crate::render_helpers::border::BorderRenderElement;
use crate::render_helpers::clipped_surface::ClippedSurfaceRenderElement;
use crate::render_helpers::renderer::NiriRenderer;
use crate::render_helpers::shadow::ShadowRenderElement;
use crate::render_helpers::solid_color::{SolidColorBuffer, SolidColorRenderElement};
use crate::render_helpers::RenderCtx;
use crate::utils::{output_size, ResizeEdge};
use crate::window::mapped::MappedId;
use crate::window::Mapped;

const CLOSE_BUTTON_SIZE: f64 = 24.;
const CLOSE_BUTTON_INSET: f64 = 8.;
const MARGIN: f64 = 16.;
const MIN_PIP_LONGEST_EDGE: f64 = 96.;
const OUTPUT_FRACTION: f64 = 0.25;
const PIP_SHADOW_ALPHA: u8 = 0x50;
const PIP_SHADOW_SOFTNESS: f64 = 18.;
const PIP_SHADOW_SPREAD: f64 = 2.;
const PIP_SHADOW_Y_OFFSET: f64 = 2.;
const RESIZE_CORNER_SIZE: f64 = 20.;

pub type PipId = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipHitTarget {
    Body,
    CloseButton,
    Resize(ResizeEdge),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PipHit {
    pub id: PipId,
    pub target: PipHitTarget,
}

#[derive(Debug, Clone)]
pub struct PipThumbnail {
    pub id: PipId,
    pub source_id: MappedId,
    pub output: Output,
    pub position: Point<f64, Logical>,
    pub size: Size<f64, Logical>,
    pub source_size: Size<f64, Logical>,
    pub is_hovered: bool,
}

impl PipThumbnail {
    fn rect(&self) -> Rectangle<f64, Logical> {
        Rectangle::new(self.position, self.size)
    }

    fn close_button_rect(&self) -> Rectangle<f64, Logical> {
        Rectangle::new(
            self.position + Point::new(CLOSE_BUTTON_INSET, CLOSE_BUTTON_INSET),
            Size::from((CLOSE_BUTTON_SIZE, CLOSE_BUTTON_SIZE)),
        )
    }

    fn resize_corner_at(&self, pos: Point<f64, Logical>) -> Option<ResizeEdge> {
        let pos = pos - self.position;
        if !Rectangle::from_size(self.size).contains(pos) {
            return None;
        }

        let corner_width = f64::max(1., f64::min(self.size.w / 3., RESIZE_CORNER_SIZE));
        let corner_height = f64::max(1., f64::min(self.size.h / 3., RESIZE_CORNER_SIZE));

        let left = pos.x <= corner_width;
        let right = pos.x >= self.size.w - corner_width;
        let top = pos.y <= corner_height;
        let bottom = pos.y >= self.size.h - corner_height;

        match (left, right, top, bottom) {
            (true, false, true, false) => Some(ResizeEdge::TOP_LEFT),
            (false, true, true, false) => Some(ResizeEdge::TOP_RIGHT),
            (true, false, false, true) => Some(ResizeEdge::BOTTOM_LEFT),
            (false, true, false, true) => Some(ResizeEdge::BOTTOM_RIGHT),
            _ => None,
        }
    }

    fn opposite_corner(&self, edges: ResizeEdge) -> Point<f64, Logical> {
        Point::new(
            if edges.contains(ResizeEdge::LEFT) {
                self.position.x + self.size.w
            } else {
                self.position.x
            },
            if edges.contains(ResizeEdge::TOP) {
                self.position.y + self.size.h
            } else {
                self.position.y
            },
        )
    }

    fn clamp_to_output(&mut self) -> bool {
        let output_size = output_size(&self.output);
        let max_x = f64::max(0., output_size.w - self.size.w);
        let max_y = f64::max(0., output_size.h - self.size.h);
        let new_position = Point::new(
            self.position.x.clamp(0., max_x),
            self.position.y.clamp(0., max_y),
        );
        let changed = self.position != new_position;
        self.position = new_position;
        changed
    }
}

niri_render_elements! {
    PipThumbnailRenderElement<R> => {
        LayoutElement = LayoutElementRenderElement<R>,
        ClippedSurface = ClippedSurfaceRenderElement<R>,
        Border = BorderRenderElement,
    }
}

niri_render_elements! {
    PipRenderElement<R> => {
        Thumbnail = RelocateRenderElement<RescaleRenderElement<PipThumbnailRenderElement<R>>>,
        Shadow = ShadowRenderElement,
        SolidColor = SolidColorRenderElement,
    }
}

#[derive(Debug)]
pub struct PipManager {
    thumbnails: Vec<PipThumbnail>,
    next_id: PipId,
    close_button: SolidColorBuffer,
}

impl PipManager {
    pub fn new() -> Self {
        Self {
            thumbnails: Vec::new(),
            next_id: 1,
            close_button: SolidColorBuffer::new(
                Size::from((CLOSE_BUTTON_SIZE, CLOSE_BUTTON_SIZE)),
                Color32F::new(0., 0., 0., 0.75),
            ),
        }
    }

    pub fn toggle_pip(&mut self, mapped: &Mapped, output: &Output) -> Vec<Output> {
        self.toggle_pip_for_source(mapped.id(), mapped.size(), output)
    }

    pub fn toggle_pip_for_source(
        &mut self,
        source_id: MappedId,
        source_size: Size<i32, Logical>,
        output: &Output,
    ) -> Vec<Output> {
        let removed = self.remove_by_source(source_id);
        if !removed.is_empty() {
            return removed;
        }

        let output_size = output_size(output);
        let source_size = source_size.to_f64();
        let size = thumbnail_size(source_size, output_size);
        let position = Point::new(
            f64::max(0., output_size.w - size.w - MARGIN),
            f64::max(0., output_size.h - size.h - MARGIN),
        );

        let id = self.next_id;
        self.next_id += 1;

        self.thumbnails.push(PipThumbnail {
            id,
            source_id,
            output: output.clone(),
            position,
            size,
            source_size,
            is_hovered: false,
        });

        vec![output.clone()]
    }

    pub fn remove(&mut self, id: PipId) -> Vec<Output> {
        self.remove_where(|thumb| thumb.id == id)
    }

    pub fn remove_by_source(&mut self, source_id: MappedId) -> Vec<Output> {
        self.remove_where(|thumb| thumb.source_id == source_id)
    }

    pub fn remove_by_output(&mut self, output: &Output) -> Vec<Output> {
        self.remove_where(|thumb| thumb.output == *output)
    }

    pub fn find(&self, id: PipId) -> Option<&PipThumbnail> {
        self.thumbnails.iter().find(|thumb| thumb.id == id)
    }

    pub fn find_by_source(&self, source_id: MappedId) -> Option<&PipThumbnail> {
        self.thumbnails
            .iter()
            .find(|thumb| thumb.source_id == source_id)
    }

    pub fn find_mut(&mut self, id: PipId) -> Option<&mut PipThumbnail> {
        self.thumbnails.iter_mut().find(|thumb| thumb.id == id)
    }

    pub fn raise_to_top(&mut self, id: PipId) -> bool {
        let Some(idx) = self.thumbnails.iter().position(|thumb| thumb.id == id) else {
            return false;
        };
        if idx + 1 == self.thumbnails.len() {
            return false;
        }

        let thumb = self.thumbnails.remove(idx);
        self.thumbnails.push(thumb);
        true
    }

    pub fn move_pip(&mut self, id: PipId, position: Point<f64, Logical>) -> Option<Output> {
        let thumb = self.find_mut(id)?;
        thumb.position = position;
        thumb.clamp_to_output();
        Some(thumb.output.clone())
    }

    pub fn pip_under(&self, output: &Output, pos: Point<f64, Logical>) -> Option<PipId> {
        self.hit_test(output, pos).map(|hit| hit.id)
    }

    pub fn hit_test(&self, output: &Output, pos: Point<f64, Logical>) -> Option<PipHit> {
        self.thumbnails
            .iter()
            .rev()
            .find(|thumb| thumb.output == *output && thumb.rect().contains(pos))
            .map(|thumb| {
                let target = if thumb.close_button_rect().contains(pos) {
                    PipHitTarget::CloseButton
                } else if let Some(edges) = thumb.resize_corner_at(pos) {
                    PipHitTarget::Resize(edges)
                } else {
                    PipHitTarget::Body
                };

                PipHit {
                    id: thumb.id,
                    target,
                }
            })
    }

    pub fn close_button_rect(&self, id: PipId) -> Option<Rectangle<f64, Logical>> {
        self.find(id).map(PipThumbnail::close_button_rect)
    }

    pub fn resize_pip(
        &mut self,
        id: PipId,
        edges: ResizeEdge,
        pointer: Point<f64, Logical>,
    ) -> Option<Output> {
        let thumb = self.find_mut(id)?;
        let source_size = normalize_source_size(thumb.source_size, thumb.size);
        let anchor = thumb.opposite_corner(edges);
        let output_size = output_size(&thumb.output);

        let requested_box = Size::new(
            if edges.contains(ResizeEdge::LEFT) {
                (anchor.x - pointer.x).max(0.)
            } else {
                (pointer.x - anchor.x).max(0.)
            },
            if edges.contains(ResizeEdge::TOP) {
                (anchor.y - pointer.y).max(0.)
            } else {
                (pointer.y - anchor.y).max(0.)
            },
        );
        let max_box = Size::new(
            if edges.contains(ResizeEdge::LEFT) {
                anchor.x.max(0.)
            } else {
                (output_size.w - anchor.x).max(0.)
            },
            if edges.contains(ResizeEdge::TOP) {
                anchor.y.max(0.)
            } else {
                (output_size.h - anchor.y).max(0.)
            },
        );

        let max_scale = scale_to_fit(source_size, max_box);
        let min_scale = min_thumbnail_scale(source_size).min(max_scale);
        let scale = scale_to_fit(source_size, requested_box).clamp(min_scale, max_scale);
        let size = source_size.upscale(scale);
        let position = Point::new(
            if edges.contains(ResizeEdge::LEFT) {
                anchor.x - size.w
            } else {
                anchor.x
            },
            if edges.contains(ResizeEdge::TOP) {
                anchor.y - size.h
            } else {
                anchor.y
            },
        );

        thumb.size = size;
        thumb.position = position;
        thumb.clamp_to_output();
        Some(thumb.output.clone())
    }

    pub fn resize_by_factor(&mut self, id: PipId, factor: f64) -> Option<Output> {
        if factor <= 0. {
            return None;
        }

        let thumb = self.find_mut(id)?;
        let source_size = normalize_source_size(thumb.source_size, thumb.size);
        let center = thumb.position + thumb.size.downscale(2.).to_point();
        let output_size = output_size(&thumb.output);
        let max_box = Size::new(
            2. * f64::min(center.x, output_size.w - center.x).max(0.),
            2. * f64::min(center.y, output_size.h - center.y).max(0.),
        );

        let max_scale = scale_to_fit(source_size, max_box);
        let min_scale = min_thumbnail_scale(source_size).min(max_scale);
        let scale = (current_scale(source_size, thumb.size) * factor).clamp(min_scale, max_scale);
        let size = source_size.upscale(scale);
        let position = center - size.downscale(2.).to_point();

        thumb.size = size;
        thumb.position = position;
        thumb.clamp_to_output();
        Some(thumb.output.clone())
    }

    pub fn update_hover(&mut self, pointer: Option<(Output, Point<f64, Logical>)>) -> bool {
        let hovered = pointer
            .as_ref()
            .and_then(|(output, pos)| self.pip_under(output, *pos));
        let mut changed = false;

        for thumb in &mut self.thumbnails {
            let is_hovered = Some(thumb.id) == hovered;
            if thumb.is_hovered != is_hovered {
                thumb.is_hovered = is_hovered;
                changed = true;
            }
        }

        changed
    }

    pub fn refresh_source_size(&mut self, source_id: MappedId, size: Size<i32, Logical>) {
        let size = size.to_f64();

        for thumb in self
            .thumbnails
            .iter_mut()
            .filter(|thumb| thumb.source_id == source_id)
        {
            thumb.source_size = size;
            thumb.size = fit_into_box(size, thumb.size);
            thumb.clamp_to_output();
        }
    }

    pub fn outputs_for_source(&self, source_id: MappedId) -> Vec<Output> {
        let mut outputs = Vec::new();

        for output in self
            .thumbnails
            .iter()
            .filter(|thumb| thumb.source_id == source_id)
            .map(|thumb| thumb.output.clone())
        {
            push_unique_output(&mut outputs, output);
        }

        outputs
    }

    pub fn clamp_output(&mut self, output: &Output) -> bool {
        let mut changed = false;
        for thumb in self
            .thumbnails
            .iter_mut()
            .filter(|thumb| thumb.output == *output)
        {
            changed |= thumb.clamp_to_output();
        }
        changed
    }

    pub fn render_for_output<R: NiriRenderer>(
        &self,
        niri: &Niri,
        output: &Output,
        mut ctx: RenderCtx<R>,
        push: &mut dyn FnMut(PipRenderElement<R>),
    ) {
        let scale = output.current_scale().fractional_scale();
        let scale = Scale::from(scale);
        let has_border_shader = BorderRenderElement::has_shader(ctx.renderer);
        let clip_shader = ClippedSurfaceRenderElement::shader(ctx.renderer).cloned();

        for thumb in self
            .thumbnails
            .iter()
            .rev()
            .filter(|thumb| thumb.output == *output)
        {
            if thumb.is_hovered {
                let elem = SolidColorRenderElement::from_buffer(
                    &self.close_button,
                    thumb.position,
                    1.,
                    Kind::Unspecified,
                );
                push(elem.into());
            }

            if thumb.source_size.w <= 0. || thumb.source_size.h <= 0. {
                continue;
            }

            let Some((_, mapped)) = niri
                .layout
                .windows()
                .find(|(_, mapped)| mapped.id() == thumb.source_id)
            else {
                continue;
            };

            let radius = if mapped.sizing_mode().is_normal() {
                mapped.rules().geometry_corner_radius
            } else {
                None
            }
            .unwrap_or_default();

            let geo = Rectangle::from_size(thumb.source_size);
            let thumb_scale = Scale {
                x: thumb.size.w / geo.size.w,
                y: thumb.size.h / geo.size.h,
            };
            let shadow_scale = thumb_scale.x.min(thumb_scale.y) as f32;

            mapped.render_normal(
                ctx.r(),
                Point::new(0., 0.),
                scale,
                1.,
                &mut |elem| {
                    let elem = clip_element(
                        elem,
                        scale,
                        geo,
                        radius,
                        has_border_shader,
                        clip_shader.clone(),
                    );
                    let elem =
                        RescaleRenderElement::from_element(elem, Point::new(0, 0), thumb_scale);
                    let elem = RelocateRenderElement::from_element(
                        elem,
                        thumb.position.to_physical_precise_round(scale),
                        Relocate::Relative,
                    );
                    push(PipRenderElement::Thumbnail(elem));
                },
            );

            let mut shadow = LayoutShadow::new(pip_shadow_config());
            shadow.update_render_elements(
                thumb.size,
                true,
                scale_corner_radius(radius, shadow_scale),
                scale.x,
                1.,
            );
            shadow.render(ctx.renderer, thumb.position, &mut |elem| {
                push(PipRenderElement::Shadow(elem));
            });
        }
    }

    fn remove_where(&mut self, mut predicate: impl FnMut(&PipThumbnail) -> bool) -> Vec<Output> {
        let mut removed = Vec::new();
        self.thumbnails.retain(|thumb| {
            if predicate(thumb) {
                push_unique_output(&mut removed, thumb.output.clone());
                false
            } else {
                true
            }
        });
        removed
    }
}

fn clip_element<R: NiriRenderer>(
    elem: LayoutElementRenderElement<R>,
    scale: Scale<f64>,
    geo: Rectangle<f64, Logical>,
    radius: CornerRadius,
    has_border_shader: bool,
    clip_shader: Option<GlesTexProgram>,
) -> PipThumbnailRenderElement<R> {
    match elem {
        LayoutElementRenderElement::Wayland(elem) => {
            if let Some(shader) = clip_shader {
                if ClippedSurfaceRenderElement::will_clip(&elem, scale, geo, radius) {
                    let elem = ClippedSurfaceRenderElement::new(elem, scale, geo, shader, radius);
                    return PipThumbnailRenderElement::ClippedSurface(elem);
                }
            }

            PipThumbnailRenderElement::LayoutElement(LayoutElementRenderElement::Wayland(elem))
        }
        LayoutElementRenderElement::SolidColor(elem) => {
            if radius != CornerRadius::default() && has_border_shader {
                return BorderRenderElement::new(
                    geo.size,
                    Rectangle::from_size(geo.size),
                    GradientInterpolation::default(),
                    Color::from_color32f(elem.color()),
                    Color::from_color32f(elem.color()),
                    0.,
                    Rectangle::from_size(geo.size),
                    0.,
                    radius,
                    scale.x as f32,
                    1.,
                )
                .into();
            }

            PipThumbnailRenderElement::LayoutElement(LayoutElementRenderElement::SolidColor(elem))
        }
        LayoutElementRenderElement::BackgroundEffect(elem) => {
            PipThumbnailRenderElement::LayoutElement(LayoutElementRenderElement::BackgroundEffect(elem))
        }
    }
}

fn thumbnail_size(
    source_size: Size<f64, Logical>,
    output_size: Size<f64, Logical>,
) -> Size<f64, Logical> {
    let max_box = Size::new(
        f64::max(1., output_size.w * OUTPUT_FRACTION),
        f64::max(1., output_size.h * OUTPUT_FRACTION),
    );
    fit_into_box(source_size, max_box)
}

fn fit_into_box(
    source_size: Size<f64, Logical>,
    max_box: Size<f64, Logical>,
) -> Size<f64, Logical> {
    if source_size.w <= 0. || source_size.h <= 0. {
        return Size::new(f64::max(1., max_box.w), f64::max(1., max_box.h));
    }

    let scale = f64::min(max_box.w / source_size.w, max_box.h / source_size.h).max(0.001);
    source_size.upscale(scale)
}

fn normalize_source_size(
    source_size: Size<f64, Logical>,
    fallback: Size<f64, Logical>,
) -> Size<f64, Logical> {
    if source_size.w > 0. && source_size.h > 0. {
        source_size
    } else {
        Size::new(f64::max(1., fallback.w), f64::max(1., fallback.h))
    }
}

fn scale_to_fit(source_size: Size<f64, Logical>, max_box: Size<f64, Logical>) -> f64 {
    let source_size = normalize_source_size(source_size, Size::from((1., 1.)));
    f64::min(max_box.w / source_size.w, max_box.h / source_size.h).max(0.001)
}

fn current_scale(source_size: Size<f64, Logical>, size: Size<f64, Logical>) -> f64 {
    let source_size = normalize_source_size(source_size, size);
    f64::min(size.w / source_size.w, size.h / source_size.h).max(0.001)
}

fn min_thumbnail_scale(source_size: Size<f64, Logical>) -> f64 {
    MIN_PIP_LONGEST_EDGE / f64::max(source_size.w, source_size.h).max(1.)
}

fn pip_shadow_config() -> ShadowConfig {
    ShadowConfig {
        on: true,
        offset: ShadowOffset {
            x: FloatOrInt(0.),
            y: FloatOrInt(PIP_SHADOW_Y_OFFSET),
        },
        softness: PIP_SHADOW_SOFTNESS,
        spread: PIP_SHADOW_SPREAD,
        draw_behind_window: false,
        color: Color::from_rgba8_unpremul(0, 0, 0, PIP_SHADOW_ALPHA),
        inactive_color: None,
    }
}

fn scale_corner_radius(radius: CornerRadius, scale: f32) -> CornerRadius {
    let scale = scale.max(0.);
    CornerRadius {
        top_left: radius.top_left * scale,
        top_right: radius.top_right * scale,
        bottom_right: radius.bottom_right * scale,
        bottom_left: radius.bottom_left * scale,
    }
}

fn push_unique_output(outputs: &mut Vec<Output>, output: Output) {
    if outputs.iter().any(|existing| existing == &output) {
        return;
    }
    outputs.push(output);
}
