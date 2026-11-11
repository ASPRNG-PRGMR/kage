//! Live settings shared by the inspector's controls and tuning bar.
use crate::pipeline::Tuning;

pub(super) struct Setting {
    pub name: &'static str,
    pub unit: &'static str,
    pub min: f32,
    pub max: f32,
    pub step: f32,
    pub get: fn(&Tuning) -> f32,
    pub set: fn(&mut Tuning, f32),
}

impl Setting {
    pub fn adjust(&self, tuning: &mut Tuning, direction: f32) {
        let value = ((self.get)(tuning) + direction * self.step).clamp(self.min, self.max);
        (self.set)(tuning, value);
    }
}

pub(super) const SETTINGS: [Setting; 4] = [
    Setting { name: "Luma", unit: "px", min: 0.25, max: 3.0, step: 0.05,
        get: |t| t.render.luma_footprint, set: |t, v| t.render.luma_footprint = v },
    Setting { name: "Chroma", unit: "px", min: 0.25, max: 3.0, step: 0.05,
        get: |t| t.render.chroma_footprint, set: |t, v| t.render.chroma_footprint = v },
    Setting { name: "Edge", unit: "", min: 0.0, max: 1.0, step: 0.05,
        get: |t| t.render.edge_suppression, set: |t, v| t.render.edge_suppression = v },
    Setting { name: "Distance", unit: "mm", min: 200.0, max: 1500.0, step: 25.0,
        get: |t| t.viewing_distance_mm, set: |t, v| t.viewing_distance_mm = v },
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{gpu::Backend, render::RenderParams};

    #[test]
    fn adjustments_step_and_clamp_each_setting_without_changing_backend() {
        let mut tuning = Tuning { backend: Backend::Vulkan, render: RenderParams::default(), viewing_distance_mm: 500.0 };
        for setting in &SETTINGS {
            (setting.set)(&mut tuning, setting.min);
            setting.adjust(&mut tuning, 1.0);
            assert!(((setting.get)(&tuning) - setting.min - setting.step).abs() < 1e-5);
            setting.adjust(&mut tuning, -10000.0);
            assert_eq!((setting.get)(&tuning), setting.min);
            setting.adjust(&mut tuning, 10000.0);
            assert_eq!((setting.get)(&tuning), setting.max);
        }
        assert_eq!(tuning.backend, Backend::Vulkan);
    }
}
