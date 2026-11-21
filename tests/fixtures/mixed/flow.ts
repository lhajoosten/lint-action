function sample(x?: { v?: boolean }, y = false) {
    const nested = () => y ? 1 : 0;
    switch (nested()) {
        case 0: return x?.v ?? false;
        default: return y;
    }
}
