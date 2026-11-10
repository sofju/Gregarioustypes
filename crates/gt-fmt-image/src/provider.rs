use crate::view::ImageView;
use gt_core::CancelToken;
use gt_image::{codecs, util, DecodeOptions, Image, Kind};
use gt_ui::{Loaded, Probe, Provider, Upgrade, View, ViewEnv};
use std::path::PathBuf;

pub struct ImageProvider;

impl Provider for ImageProvider {
    fn id(&self) -> &'static str {
        "image"
    }

    fn score(&self, p: &Probe) -> u32 {
        if p.is_dir {
            return 0;
        }
        codecs::detect(&p.head, &p.ext).map(|(_, s)| s).unwrap_or(0)
    }

    fn load(&self, p: &Probe, _cancel: &CancelToken) -> Result<Box<dyn Loaded>, String> {
        let (kind, _) = codecs::detect(&p.head, &p.ext).ok_or("not an image")?;
        let (sw, sh) = if p.screen.0 > 0 { p.screen } else { (3840, 2160) };
        let opts = DecodeOptions { target_w: sw, target_h: sh, ..Default::default() };
        let img = codecs::decode(&p.path, kind, &opts).map_err(|e| e.to_string())?;
        let peak = util::content_peak_nits(&img);
        Ok(Box::new(ImageLoaded { img: Some(img), peak, path: p.path.clone(), kind, screen: (sw, sh) }))
    }
}

pub struct ImageLoaded {
    img: Option<Image>,
    peak: f32,
    path: PathBuf,
    kind: Kind,
    screen: (u32, u32),
}

/// A full-resolution decode replacing a reduced first stage.
pub struct FullImage {
    pub img: Image,
    pub peak: f32,
}

impl Loaded for ImageLoaded {
    fn into_view(mut self: Box<Self>, env: &ViewEnv) -> Box<dyn View> {
        let img = self.img.take().expect("image");
        Box::new(ImageView::new(env, img, self.peak))
    }

    fn take_upgrade(&mut self) -> Option<Upgrade> {
        let img = self.img.as_ref()?;
        let reduced = img.full_size.is_some();
        if !reduced || !matches!(self.kind, Kind::Jpeg | Kind::J2k | Kind::Raw) {
            return None;
        }
        let (path, kind, screen) = (self.path.clone(), self.kind, self.screen);
        Some(Box::new(move |cancel: &CancelToken| {
            if cancel.is_cancelled() {
                return None;
            }
            let opts = DecodeOptions { target_w: screen.0, target_h: screen.1, full: true, anim_budget: 0 };
            let img = codecs::decode(&path, kind, &opts).ok()?;
            if cancel.is_cancelled() {
                return None;
            }
            let peak = util::content_peak_nits(&img);
            Some(Box::new(FullImage { img, peak }) as Box<dyn std::any::Any + Send>)
        }))
    }
}
