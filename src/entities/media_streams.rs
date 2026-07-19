use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "media_streams")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    pub item_id: String,
    pub stream_index: i64,
    pub stream_type: String,
    pub codec: Option<String>,
    pub profile: Option<String>,
    pub codec_tag: Option<String>,
    pub language: Option<String>,
    pub title: Option<String>,
    pub comment: Option<String>,
    pub bit_rate: Option<i64>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub aspect_ratio: Option<String>,
    pub average_frame_rate: Option<f64>,
    pub real_frame_rate: Option<f64>,
    pub reference_frame_rate: Option<f64>,
    pub channels: Option<i64>,
    pub channel_layout: Option<String>,
    pub sample_rate: Option<i64>,
    pub bit_depth: Option<i64>,
    pub ref_frames: Option<i64>,
    pub is_interlaced: i64,
    pub is_avc: Option<i64>,
    pub is_anamorphic: Option<i64>,
    pub pixel_format: Option<String>,
    pub level: Option<i64>,
    pub color_range: Option<String>,
    pub color_space: Option<String>,
    pub color_transfer: Option<String>,
    pub color_primaries: Option<String>,
    pub time_base: Option<String>,
    pub codec_time_base: Option<String>,
    pub nal_length_size: Option<String>,
    pub rotation: Option<i64>,
    pub video_range: Option<String>,
    pub video_range_type: Option<String>,
    pub hdr10_plus_present_flag: Option<i64>,
    pub is_default: i64,
    pub is_forced: i64,
    pub is_hearing_impaired: i64,
    pub is_original: Option<i64>,
    pub path: Option<String>,
    pub is_external: i64,
    pub created_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
