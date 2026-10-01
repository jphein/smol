// ES8311 Audio codec (speaker DAC — the mics are on the ES7210). The driver is smol's one copy,
// `rust/es8311`, shared with the S3 tapstone station; it moved there byte for byte (that crate's
// `c6_watch_sequence_is_unchanged` test pins every I2C transaction this watch makes). Playback
// data rides the shared I2S TX ring (audio_out/silent_clock_task, #23); the driver only sequences
// codec power: unmute() before the amp rises, shutdown() after it drops (see service_amp).
pub use es8311::Es8311;

// fill_beep_buffer (stereo square-wave synth) retired in v0.8.5: SFX are now
// synthesized MONO via mic-dsp (fill_tone_mono_s16le / fill_click_mono_s16le,
// host-unit-tested) and expanded to stereo by the audio_out feeder (#23).
