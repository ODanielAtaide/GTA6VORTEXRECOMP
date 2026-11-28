// SPDX-License-Identifier: GPL-3.0-or-later
#include "vortex_root_signature.h"

#include <bit>
#include <cstring>
#include <span>
#include <utility>

namespace rt::rootsig {
namespace {

// VRS1 is the pointer-free wire representation emitted by root_native.rs.
class Words {
    std::span<const uint8_t> bytes_;
public:
    explicit Words(std::span<const uint8_t> bytes) : bytes_(bytes) {}
    std::span<const uint8_t> take(size_t count) {
        if (count > bytes_.size() / 4) return {};
        auto result = bytes_.first(count * 4);
        bytes_ = bytes_.subspan(count * 4);
        return result;
    }
    bool empty() const { return bytes_.empty(); }
};

uint32_t word(std::span<const uint8_t> bytes, size_t index) {
    uint32_t result;
    std::memcpy(&result, bytes.data() + index * 4, 4);
    static_assert(std::endian::native == std::endian::little);
    return result;
}

DecodeResult invalid(std::string error) {
    DecodeResult result{};
    result.error = std::move(error);
    return result;
}

}  // namespace

DecodeResult decode_vortex_root(const uint8_t* data, size_t size) {
    if (size < 4 || std::memcmp(data, "VRS1", 4) != 0) return parse_rts0(data, size);
    if (size % 4 || size > 4 * 1048576) return invalid("invalid VRS1 size");
    Words input({data, size});
    const auto header = input.take(5);
    if (header.empty()) return invalid("truncated VRS1 header");
    const auto version = word(header, 1);
    const auto parameters = word(header, 3);
    const auto samplers = word(header, 4);
    if (version < 1 || version > 3 || parameters > 64 || samplers > 4096)
        return invalid("unsupported VRS1 version or count");
    RootSignature root{};
    root.version = static_cast<Version>(version);
    root.flags = word(header, 2);
    root.params.reserve(parameters);
    for (uint32_t index = 0; index < parameters; ++index) {
        const auto parameter = input.take(5);
        if (parameter.empty()) return invalid("truncated VRS1 parameter");
        RootParameter param{};
        const auto type = word(parameter, 0);
        const auto visibility = word(parameter, 1);
        if (visibility > 7 || type > 4) return invalid("invalid VRS1 root parameter");
        param.visibility = static_cast<Visibility>(visibility);
        switch (type) {
        case 0: {
            param.type = ParamType::Table;
            const auto count = word(parameter, 2);
            const auto stride = version == 1 ? 5u : 6u;
            const auto ranges = input.take(size_t(count) * stride);
            if (count && ranges.empty()) return invalid("truncated VRS1 ranges");
            for (uint32_t r = 0; r < count; ++r) {
                const auto range = ranges.subspan(size_t(r) * stride * 4, size_t(stride) * 4);
                const auto kind = word(range, 0);
                if (kind > 3) return invalid("invalid VRS1 range type");
                param.ranges.push_back({kind, word(range, 1), word(range, 2), word(range, 3),
                    version == 1 ? 0 : word(range, 4), word(range, stride - 1)});
            }
            break;
        }
        case 1:
            param.type = ParamType::Constants;
            param.shader_register = word(parameter, 2);
            param.register_space = word(parameter, 3);
            param.num_values = word(parameter, 4);
            break;
        default:
            param.type = ParamType::Descriptor;
            param.descriptor_register = word(parameter, 2);
            param.descriptor_space = word(parameter, 3);
            break;
        }
        root.params.push_back(std::move(param));
    }
    root.static_samplers.reserve(samplers);
    for (uint32_t index = 0; index < samplers; ++index) {
        const auto data = input.take(version == 3 ? 14 : 13);
        if (data.empty() || word(data, 12) > 7) return invalid("invalid VRS1 static sampler");
        root.static_samplers.push_back({
            word(data, 0), word(data, 1), word(data, 2), word(data, 3),
            std::bit_cast<float>(word(data, 4)), word(data, 5), word(data, 6), word(data, 7),
            std::bit_cast<float>(word(data, 8)), std::bit_cast<float>(word(data, 9)),
            word(data, 10), word(data, 11), static_cast<Visibility>(word(data, 12)),
            version == 3 ? word(data, 13) : 0,
        });
    }
    if (!input.empty()) return invalid("trailing data in VRS1 root signature");
    DecodeResult result{};
    result.ok = true;
    result.sig = std::move(root);
    return result;
}
}  // namespace rt::rootsig
