package comart.rudbman.bridge.support;

import org.h2.api.Trigger;

import java.sql.Connection;

/**
 * What an H2 schema's routines and triggers call, so that a test can create
 * them: H2 implements both in Java, by class name.
 */
public final class H2Objects {

    private H2Objects() {
    }

    /**
     * The body of {@code CREATE ALIAS ... FOR 'comart.rudbman.bridge.support.H2Objects.half'}.
     *
     * @param x a number
     * @return half of it, rounded down
     */
    public static int half(int x) {
        return x / 2;
    }

    /** A trigger that does nothing, for a schema that has to have one. */
    public static final class Noop implements Trigger {
        @Override
        public void fire(Connection conn, Object[] oldRow, Object[] newRow) {
            // Nothing: the trigger only has to exist.
        }
    }
}
