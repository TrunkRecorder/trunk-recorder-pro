// Trunk Recorder's two IMBE decoders on a frame file.
//
//   imbe_tr float <frames.jsonl> <out.s16> [name=value ...]   TR's float decoder (software_imbe_decoder)
//   imbe_tr fixed <frames.jsonl> <out.s16>                    TR's fixed-point decoder (imbe_vocoder, Pavel Yazev)
//   imbe_tr stats <frames.jsonl> [name=value ...]             voicing statistics as one JSON line
//
// Frames are trunk-pro's .frames.jsonl records ({"codec":"imbe","bits":<22 hex>,"e0":n,"errs":n,...});
// other codecs are skipped. Output is raw 16-bit 8 kHz mono. name=value sets any VocoderParams
// field of the float decoder (see software_imbe_decoder.h), e.g. aper_max=0.5 hf_lift_db=0.
//
// Built against a Trunk Recorder source tree by shootout.py setup; both projects are GPLv3.

#define private public
#define protected public
#include "software_imbe_decoder.h"
#undef private
#undef protected
#include "imbe_vocoder/imbe_vocoder.h"

#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <fstream>
#include <string>

static bool set_param(VocoderParams &p, const std::string &k, double v) {
#define F(n) if (k == #n) { p.n = (decltype(p.n))v; return true; }
	F(fmt_alpha) F(fmt_w) F(phase_c_env) F(phase_w_rand) F(phase_low_blend) F(phase_kernel) F(phase_kernel_d)
	F(phase_kernel_gamma) F(voicing_smooth_taps) F(voicing_smooth_er_threshold) F(uv_to_v_reset) F(interp_max_l)
	F(interp_pitch_tol) F(phase_track) F(amp_smooth) F(uv_synth_mode) F(uv_smooth_gain) F(uv_xfade) F(aper_max)
	F(aper_f1) F(aper_f2) F(hf_lift_db) F(hf_lift_f1) F(tap_gain) F(onset_ramp_mode) F(repeat_amplitude_decay)
	F(mute_er) F(repeat_e0) F(repeat_et_base) F(repeat_et_slope) F(max_repeats)
#undef F
	return false;
}

struct Frame { uint32_t u[8]; uint32_t e0, et; };

// One record → u0..u7 (u7 stored <<1, as imbe_header_decode returns it). False: not an IMBE frame.
static bool parse(const std::string &line, Frame &f) {
	if (line.find("\"codec\":\"imbe\"") == std::string::npos) return false;
	auto p = line.find("\"bits\":\"");
	if (p == std::string::npos || p + 8 + 22 > line.size()) return false;
	int bits[88];
	for (int i = 0; i < 11; i++) {
		unsigned v = std::stoul(line.substr(p + 8 + 2 * i, 2), nullptr, 16);
		for (int b = 0; b < 8; b++) bits[i * 8 + b] = (v >> (7 - b)) & 1;
	}
	static const int W[8] = {12, 12, 12, 12, 11, 11, 11, 7};
	int k = 0;
	for (int j = 0; j < 8; j++) {
		f.u[j] = 0;
		for (int b = 0; b < W[j]; b++) f.u[j] = (f.u[j] << 1) | bits[k++];
	}
	f.u[7] <<= 1;
	auto num = [&](const char *key) -> uint32_t {
		auto q = line.find(key);
		return q == std::string::npos ? 0 : (uint32_t)atoi(line.c_str() + q + strlen(key));
	};
	bool erased = line.find("\"erased\":true") != std::string::npos;
	f.e0 = erased ? 99 : num("\"e0\":");
	f.et = erased ? 99 : num("\"errs\":");
	return true;
}

int main(int argc, char **argv) {
	if (argc < 3) {
		fprintf(stderr, "usage: imbe_tr float|fixed <frames.jsonl> <out.s16> [name=value ...]\n       imbe_tr stats <frames.jsonl> [name=value ...]\n");
		return 2;
	}
	std::string mode = argv[1];
	bool stats = mode == "stats";
	if (mode != "float" && mode != "fixed" && !stats) { fprintf(stderr, "imbe_tr: unknown mode %s\n", mode.c_str()); return 2; }
	if (!stats && argc < 4) { fprintf(stderr, "imbe_tr: no output file\n"); return 2; }
	std::ifstream in(argv[2]);
	if (!in) { fprintf(stderr, "imbe_tr: can't read %s\n", argv[2]); return 1; }
	FILE *out = stats ? nullptr : fopen(argv[3], "wb");
	if (!stats && !out) { fprintf(stderr, "imbe_tr: can't write %s\n", argv[3]); return 1; }

	software_imbe_decoder d;
	d.clear();
	imbe_vocoder fx;
	VocoderParams p = d.get_params();
	for (int a = stats ? 3 : 4; a < argc; a++) {
		std::string kv = argv[a];
		auto e = kv.find('=');
		if (e == std::string::npos || !set_param(p, kv.substr(0, e), atof(kv.c_str() + e + 1))) {
			fprintf(stderr, "imbe_tr: unknown setting %s\n", kv.c_str());
			return 2;
		}
	}
	d.set_params(p);

	long frames = 0, speech = 0, voiced_frames = 0, fully = 0, harm = 0, harm_v = 0, hi = 0, hi_v = 0;
	double f0 = 0;
	std::string line;
	Frame f;
	int16_t s[160];
	while (std::getline(in, line)) {
		if (!parse(line, f)) continue;
		frames++;
		if (mode == "fixed") {
			int16_t fv[8];
			for (int i = 0; i < 8; i++) fv[i] = f.u[i] & 0xFFFF;
			fv[7] >>= 1;
			fx.imbe_decode_checked(fv, f.e0, f.et, s);
		} else {
			d.decode_fullrate(s, f.u[0], f.u[1], f.u[2], f.u[3], f.u[4], f.u[5], f.u[6], f.u[7], f.e0, f.et);
		}
		if (stats) {
			// After a decode the frame's parameters are in the "Old" slot.
			int O = d.Old, v = 0;
			if (d.L > 8) {
				speech++;
				for (int l = 1; l <= d.L; l++) {
					int vv = d.vee[l][O] ? 1 : 0;
					v += vv;
					harm++;
					harm_v += vv;
					if (l * d.Oldw0 * 8000 / (2 * M_PI) > 2000) { hi++; hi_v += vv; }
				}
				if (v == d.L) fully++;
				if (v > 0) { voiced_frames++; f0 += d.Oldw0 * 8000 / (2 * M_PI); }
			}
		} else {
			fwrite(s, 2, 160, out);
		}
	}
	if (stats) {
		auto r = [](long a, long b) { return b ? (double)a / b : 0.0; };
		printf("{\"frames\":%ld,\"speech_frames\":%ld,\"voiced_frames\":%.4f,\"fully_voiced\":%.4f,\"harmonics_voiced\":%.4f,\"above_2k_voiced\":%.4f,\"mean_f0_hz\":%.1f}\n",
		       frames, speech, r(voiced_frames, speech), r(fully, speech), r(harm_v, harm), r(hi_v, hi), voiced_frames ? f0 / voiced_frames : 0.0);
	} else {
		fclose(out);
	}
	return 0;
}
