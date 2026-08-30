//! One image layer's channels: colour, contrast, opacity.

use yew::prelude::*;

use crate::layers::LayerUi;

use super::{App, AppMsg};

pub enum ChannelMsg {
    Visibility(usize, usize, bool),
    Color(usize, usize, [f32; 3]),
    ContrastMin(usize, usize, f32),
    ContrastMax(usize, usize, f32),
    Opacity(usize, usize, f32),
}

impl From<ChannelMsg> for AppMsg {
    fn from(msg: ChannelMsg) -> Self {
        AppMsg::Channel(msg)
    }
}

impl App {
    pub(super) fn update_channels(&mut self, ctx: &Context<Self>, msg: ChannelMsg) -> bool {
        match msg {
            ChannelMsg::Visibility(layer, channel, visible) => {
                if let Some(ch) = self.channel_mut(layer, channel) {
                    ch.visible = visible;
                    ch.opacity = if visible { 1.0 } else { 0.0 };
                }
                self.load_tiles(ctx);
                true
            }
            ChannelMsg::Color(layer, channel, color) => {
                if let Some(ch) = self.channel_mut(layer, channel) {
                    ch.color = color;
                }
                true
            }
            ChannelMsg::ContrastMin(layer, channel, value) => {
                if let Some(ch) = self.channel_mut(layer, channel) {
                    ch.contrast_min = value;
                }
                true
            }
            ChannelMsg::ContrastMax(layer, channel, value) => {
                if let Some(ch) = self.channel_mut(layer, channel) {
                    ch.contrast_max = value;
                }
                true
            }
            ChannelMsg::Opacity(layer, channel, value) => {
                if let Some(ch) = self.channel_mut(layer, channel) {
                    ch.opacity = value;
                }
                true
            }
        }
    }
}

impl App {
    pub(super) fn channel_mut(
        &mut self,
        layer: usize,
        channel: usize,
    ) -> Option<&mut crate::layers::ChannelUiState> {
        match &mut self.layers.get_mut(layer)?.ui {
            LayerUi::Image { channels, .. } => channels.get_mut(channel),
            _ => None,
        }
    }
}
