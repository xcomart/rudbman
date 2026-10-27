package comart.rudbman.bridge.job;

import java.io.IOException;
import java.io.OutputStream;
import java.nio.file.AtomicMoveNotSupportedException;
import java.nio.file.FileAlreadyExistsException;
import java.nio.file.FileSystemException;
import java.nio.file.Files;
import java.nio.file.NoSuchFileException;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;
import java.nio.file.StandardOpenOption;
import java.util.concurrent.ThreadLocalRandom;
import java.util.logging.Level;
import java.util.logging.Logger;

/**
 * The file a script-writing job writes to: a hidden sibling of the target that
 * only becomes the target once the job has finished.
 *
 * <p>Shared by {@link ExtractJob} and {@link BackupJob}. Opening the target
 * itself would truncate it before the first row is read, so a backup that fails
 * or is canceled half way would take the previous good backup down with it.
 * Writing beside it and renaming on success means the target is only ever the
 * old file or the complete new one: {@link #commit()} moves the sibling over it
 * in one rename, and {@link #discard()} deletes the sibling and leaves the old
 * file alone.
 *
 * <p>The sibling lives in the target's own directory, not in the system temp
 * directory, because a rename is only atomic - or even possible without a copy -
 * within one file system.
 */
final class OutputFile {

    private static final Logger LOG = Logger.getLogger(OutputFile.class.getName());

    /** How many names {@link #create} tries before it gives up. */
    private static final int ATTEMPTS = 16;

    private final Path target;
    private final Path temp;

    private OutputFile(Path target, Path temp) {
        this.target = target;
        this.temp = temp;
    }

    /**
     * Creates the target's directory, if it is missing, and an empty sibling to
     * write into.
     *
     * <p>A target that is a symbolic link is resolved first, so that the
     * finished file replaces what the link points to rather than the link. An
     * existing target's POSIX permissions are copied onto the sibling, so that
     * replacing a file the user had locked down does not loosen it.
     *
     * @param requested the path the caller asked for
     * @return the new output file
     * @throws IOException if the directory or the sibling cannot be created
     */
    static OutputFile create(Path requested) throws IOException {
        Path target = requested.toAbsolutePath();
        if (Files.isSymbolicLink(target)) {
            try {
                target = target.toRealPath();
            } catch (NoSuchFileException dangling) {
                // A link to nothing: the finished file replaces the link.
            }
        }
        if (Files.isDirectory(target)) {
            // Found now rather than by the rename at the very end, after the
            // whole job has been spent on a file that cannot land.
            throw new FileSystemException(target.toString(), null, "is a directory");
        }
        Path dir = target.getParent();
        if (dir != null) {
            Files.createDirectories(dir);
        }
        String name = target.getFileName().toString();
        for (int i = 0; ; i++) {
            Path temp = target.resolveSibling("." + name + "."
                    + Long.toHexString(ThreadLocalRandom.current().nextLong()) + ".tmp");
            try {
                Files.createFile(temp);
            } catch (FileAlreadyExistsException taken) {
                if (i + 1 >= ATTEMPTS) {
                    throw taken;
                }
                continue;
            }
            if (Files.exists(target)) {
                try {
                    Files.setPosixFilePermissions(temp, Files.getPosixFilePermissions(target));
                } catch (UnsupportedOperationException | IOException e) {
                    // Not a POSIX file system, or not ours to read: the default
                    // permissions are what a fresh file would have had anyway.
                    LOG.log(Level.FINE, "cannot copy permissions of " + target, e);
                }
            }
            return new OutputFile(target, temp);
        }
    }

    /**
     * @return a stream over the sibling; closing it is the caller's job, and
     *         has to happen before {@link #commit()} or {@link #discard()}
     * @throws IOException if the sibling cannot be opened
     */
    OutputStream open() throws IOException {
        return Files.newOutputStream(temp, StandardOpenOption.WRITE,
                StandardOpenOption.TRUNCATE_EXISTING);
    }

    /**
     * Moves the finished sibling over the target.
     *
     * @throws IOException if the rename fails; the sibling is then still there
     *                     and {@link #discard()} removes it
     */
    void commit() throws IOException {
        try {
            Files.move(temp, target, StandardCopyOption.ATOMIC_MOVE,
                    StandardCopyOption.REPLACE_EXISTING);
        } catch (AtomicMoveNotSupportedException e) {
            // The same directory, so this is unusual; a plain replace is still
            // better than failing a job whose output is complete.
            Files.move(temp, target, StandardCopyOption.REPLACE_EXISTING);
        }
    }

    /** Deletes the sibling, leaving the target as it was. Never throws. */
    void discard() {
        try {
            Files.deleteIfExists(temp);
        } catch (IOException e) {
            LOG.log(Level.WARNING, "cannot delete unfinished output " + temp, e);
        }
    }
}
