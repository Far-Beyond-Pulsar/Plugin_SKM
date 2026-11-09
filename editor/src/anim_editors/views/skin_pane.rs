//! The skinned preview as an editor panel: the rig of the open asset's
//! skeleton, an offscreen renderer and the last frame as an image. The pose
//! comes from the caller (the Montage or Blend Space runtime preview).

use std::path::Path;
use std::sync::Arc;

use gpui::*;
use skeletal_assets::runtime::pose::Pose;
use ui::{v_flex, ActiveTheme, StyledExt};

use crate::anim_editors::graph_preview::ProjectAssets;
use crate::anim_editors::skin_preview::{load_rig, Rig, SkinRenderer};

pub const WIDTH: u32 = 320;
pub const HEIGHT: u32 = 260;

#[derive(Default)]
pub struct SkinPane {
    /// `None` until first used, `Some(None)` when the machine has no GPU.
    renderer: Option<Option<SkinRenderer>>,
    rig: Option<(String, Result<Rig, String>)>,
    image: Option<Arc<RenderImage>>,
    drawn_key: Option<u64>,
}

impl SkinPane {
    /// The rig of `skeleton` (loaded once per skeleton), or why there is none.
    pub fn rig(&mut self, root: &Path, skeleton: Option<&str>) -> Result<&Rig, String> {
        let Some(id) = skeleton else { return Err("Pick a skeleton to preview.".into()) };
        if self.rig.as_ref().is_none_or(|(loaded, _)| loaded != id) {
            self.rig = Some((id.to_string(), load_rig(root, &ProjectAssets::new(root), id)));
            self.image = None;
            self.drawn_key = None;
        }
        match &self.rig {
            Some((_, Ok(rig))) => Ok(rig),
            Some((_, Err(e))) => Err(e.clone()),
            None => Err("No rig".into()),
        }
    }

    /// Forget the loaded rig (the skeleton or its mesh changed on disk).
    pub fn invalidate(&mut self) {
        self.rig = None;
        self.drawn_key = None;
    }

    /// Draw `pose` unless the frame for `key` is already on screen.
    pub fn draw(&mut self, pose: &Pose, key: u64, palette: &crate::style::Palette) {
        if self.drawn_key == Some(key) {
            return;
        }
        let renderer = self.renderer.get_or_insert_with(SkinRenderer::new);
        let (Some(renderer), Some((_, Ok(rig)))) = (renderer.as_mut(), self.rig.as_ref()) else { return };
        let frame = renderer.render(rig, pose, palette, WIDTH, HEIGHT);
        // gpui images are BGRA
        let mut bgra = frame.rgba;
        for px in bgra.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
        if let Some(buffer) = image::RgbaImage::from_raw(frame.width, frame.height, bgra) {
            self.image = Some(Arc::new(RenderImage::new(smallvec::smallvec![image::Frame::new(buffer)])));
            self.drawn_key = Some(key);
        }
    }

    pub fn has_gpu(&self) -> bool {
        !matches!(self.renderer, Some(None))
    }

    pub fn has_mesh(&self) -> bool {
        matches!(&self.rig, Some((_, Ok(rig))) if rig.mesh.is_some())
    }

    /// The frame, or the reason there is none.
    pub fn element(&self, problem: Option<&str>, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let message = problem.map(str::to_string).or_else(|| (!self.has_gpu()).then(|| "No GPU adapter: the skinned preview needs one.".to_string())).or_else(|| {
            matches!(&self.rig, Some((_, Ok(rig))) if rig.mesh.is_none()).then(|| "No skeletal mesh is built for this skeleton; the bones are drawn instead.".to_string())
        });
        let frame = match &self.image {
            Some(image) => img(ImageSource::Render(image.clone())).w(px(WIDTH as f32)).h(px(HEIGHT as f32)).into_any_element(),
            None => div().w(px(WIDTH as f32)).h(px(HEIGHT as f32)).bg(theme.muted).into_any_element(),
        };
        v_flex()
            .w_full()
            .items_center()
            .gap_1()
            .child(div().rounded(px(6.0)).overflow_hidden().border_1().border_color(theme.border).child(frame))
            .when_some(message, |col, text| col.child(div().text_xs().text_color(theme.muted_foreground).child(text)))
            .into_any_element()
    }
}

use gpui::prelude::FluentBuilder;
