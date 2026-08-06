package comart.rudbman.bridge.job;

import java.io.BufferedWriter;
import java.io.Closeable;
import java.io.IOException;
import java.io.OutputStream;
import java.io.OutputStreamWriter;
import java.io.Writer;
import java.nio.charset.Charset;
import java.util.zip.GZIPOutputStream;

/**
 * The output file of a script-writing job: a buffered, charset-encoding writer
 * over a stream that counts the bytes it passes on, optionally through gzip.
 *
 * <p>Shared by {@link ExtractJob} and {@link BackupJob} so that the two write
 * byte-identical files from the same generated text. The counter sits
 * <em>below</em> the compressor on purpose: {@code JOB_POLL} reports the bytes
 * that reached the file, which has to be the file's size and not the size of the
 * text that went in.
 *
 * <p>The count is read without flushing, so it lags by up to one buffer while
 * the job runs - and, with gzip, by up to one deflate window. That is the right
 * trade for a progress bar; flushing per row to make the number exact would cost
 * far more than the number is worth. It is exact once {@link #close()} has run,
 * which is why a compressed job takes its final reading after closing rather
 * than before.
 */
final class ScriptOut implements Closeable {

    /** Writer buffer, in characters. */
    private static final int BUFFER = 1 << 16;

    private final Counting counting;
    private final Writer writer;
    private final String newline;

    /** Bytes already handed to {@link Jobs.Job#addBytes}. */
    private long reported;

    /**
     * @param raw     the file stream; closed with this object
     * @param charset the encoding to write in
     * @param newline the record separator, {@code "\n"} or {@code "\r\n"}
     * @param gzip    whether to compress
     * @throws IOException if the gzip header cannot be written
     */
    ScriptOut(OutputStream raw, Charset charset, String newline, boolean gzip) throws IOException {
        this.newline = newline;
        this.counting = new Counting(raw);
        OutputStream sink = gzip ? new GZIPOutputStream(counting, BUFFER) : counting;
        this.writer = new BufferedWriter(new OutputStreamWriter(sink, charset), BUFFER);
    }

    /**
     * Writes text exactly as given, including any line breaks it holds.
     *
     * @param s the text
     * @throws IOException if the file cannot be written
     */
    void raw(String s) throws IOException {
        writer.write(s);
    }

    /**
     * Writes text and the configured record separator.
     *
     * @param s the text
     * @throws IOException if the file cannot be written
     */
    void line(String s) throws IOException {
        writer.write(s);
        writer.write(newline);
    }

    /**
     * Writes a block of generated SQL, translating its line breaks to the
     * configured separator.
     *
     * <p>Only generated text goes through here. Row data never does: a line
     * break inside a value is data, and rewriting it would corrupt the row.
     *
     * @param s the generated text
     * @throws IOException if the file cannot be written
     */
    void block(String s) throws IOException {
        String normalised = s.replace("\r\n", "\n");
        if ("\n".equals(newline)) {
            writer.write(normalised);
        } else {
            writer.write(normalised.replace("\n", newline));
        }
    }

    /**
     * @throws IOException if the file cannot be written
     */
    void flush() throws IOException {
        writer.flush();
    }

    /** @return bytes handed to the file so far, buffered text excluded */
    long written() {
        return counting.count;
    }

    /**
     * @return bytes written since the last call, for feeding
     *         {@link Jobs.Job#addBytes}
     */
    long unreported() {
        long w = written();
        long delta = w - reported;
        reported = w;
        return delta;
    }

    @Override
    public void close() throws IOException {
        writer.close();
    }

    /** An output stream that counts. */
    private static final class Counting extends OutputStream {

        private final OutputStream out;
        private volatile long count;

        Counting(OutputStream out) {
            this.out = out;
        }

        @Override
        public void write(int b) throws IOException {
            out.write(b);
            count++;
        }

        @Override
        public void write(byte[] b, int off, int len) throws IOException {
            out.write(b, off, len);
            count += len;
        }

        @Override
        public void flush() throws IOException {
            out.flush();
        }

        @Override
        public void close() throws IOException {
            out.close();
        }
    }
}
