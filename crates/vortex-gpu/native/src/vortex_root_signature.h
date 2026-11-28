// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include "root_signature.h"

namespace rt::rootsig {
[[nodiscard]] DecodeResult decode_vortex_root(const uint8_t* data, size_t size);
}
