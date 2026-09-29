CXX ?= clang++
BREW ?= /opt/homebrew
CXXFLAGS = -std=c++17 -O3 -ffast-math -mcpu=native -DHAVE_FFTW -I$(BREW)/include
LDLIBS = -L$(BREW)/lib -lfftw3f -framework Accelerate

bench: bench.cpp channelizer.hpp demod.hpp fft.hpp
	$(CXX) $(CXXFLAGS) -o $@ bench.cpp $(LDLIBS)

# Same code with auto-vectorisation off: what SIMD is worth.
bench-novec: bench.cpp channelizer.hpp demod.hpp fft.hpp
	$(CXX) $(CXXFLAGS) -fno-vectorize -fno-slp-vectorize -o $@ bench.cpp $(LDLIBS)

p25tool: p25tool.cpp channelizer.hpp demod.hpp c4fm.hpp fft.hpp p25_frame.hpp p25_tsbk.hpp p25_fec.hpp p25_voice.hpp mbe.hpp diversity.hpp tables.gen.hpp
	$(CXX) $(CXXFLAGS) -o $@ p25tool.cpp $(LDLIBS)

recorder: recorder.cpp channelizer.hpp demod.hpp c4fm.hpp fft.hpp p25_frame.hpp p25_tsbk.hpp p25_fec.hpp p25_voice.hpp mbe.hpp trunk.hpp diversity.hpp tables.gen.hpp
	$(CXX) $(CXXFLAGS) -o $@ recorder.cpp $(LDLIBS)

clean:
	rm -f bench bench-novec p25tool recorder
