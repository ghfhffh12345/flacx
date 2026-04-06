1. Support the full legal FLAC sample-rate and block-size envelope beyond the current streamable-subset-only limits (RFC 9639 Sections 7, 8.2, 9.1.2, 9.1.6, 9.1.7, Appendix C.5, Appendix C.7).
2. Add non-ordinary multichannel layout interop via `WAVEFORMATEXTENSIBLE_CHANNEL_MASK` / `WAVEFORMATEXTENSIBLE` channel-mask signaling (RFC 9639 Sections 7, 8.6.2, 9.1.3, Appendix C.7).
3. Add `SEEKTABLE` / seek-point read-write support (RFC 9639 Sections 8.5, 8.5.1).
4. Broaden `VORBIS_COMMENT` support beyond the current narrow mapped subset (RFC 9639 Sections 8.6, 8.6.1).
5. Broaden `CUESHEET` support beyond basic cue-point offsets (RFC 9639 Sections 8.7, 8.7.1).
6. Add encoder-side `PADDING` metadata block support (RFC 9639 Section 8.3).
7. Add `PICTURE` metadata block support (RFC 9639 Section 8.8).
8. Add `APPLICATION` metadata block support (RFC 9639 Section 8.4).
