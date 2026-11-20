package comart.rudbman.bridge.meta;

/**
 * A product the bridge does not recognize: every answer is {@link Vendor}'s
 * portable default.
 */
final class PortableVendor extends Vendor {

    @Override
    boolean names(String productName) {
        return false;
    }
}
