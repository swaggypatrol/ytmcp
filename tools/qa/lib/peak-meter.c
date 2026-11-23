#include <pulse/pulseaudio.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>

/* Pulse supplies a 25 Hz peak envelope. Emit level states only, never audio. */
static void report_peak(pa_stream *stream, size_t available, void *userdata) {
    (void)available;
    pa_mainloop *loop = userdata;
    const void *data;
    size_t size;
    if (pa_stream_peek(stream, &data, &size) < 0) {
        pa_mainloop_quit(loop, 1);
        return;
    }
    if (size == 0) return;
    if (data != NULL) {
        const float *peaks = data;
        bool active = false;
        for (size_t i = 0; i < size / sizeof(*peaks); i++) {
            if (peaks[i] > 0.001f) active = true;
        }
        puts(active ? "active" : "quiet");
        fflush(stdout);
    }
    if (pa_stream_drop(stream) < 0) pa_mainloop_quit(loop, 1);
}

int main(void) {
    pa_mainloop *loop = pa_mainloop_new();
    if (loop == NULL) return EXIT_FAILURE;
    pa_context *context = pa_context_new(pa_mainloop_get_api(loop), "YTM Glass QA meter");
    pa_stream *stream = NULL;
    int status = EXIT_FAILURE;
    if (context == NULL || pa_context_connect(context, NULL, 0, NULL) < 0) goto done;
    while (pa_context_get_state(context) != PA_CONTEXT_READY) {
        if (!PA_CONTEXT_IS_GOOD(pa_context_get_state(context)) ||
            pa_mainloop_iterate(loop, 1, NULL) < 0) goto done;
    }
    const pa_sample_spec spec = { PA_SAMPLE_FLOAT32NE, 25, 1 };
    const pa_buffer_attr buffer = { (uint32_t)-1, (uint32_t)-1, (uint32_t)-1, (uint32_t)-1, sizeof(float) };
    stream = pa_stream_new(context, "Null sink peak meter", &spec, NULL);
    if (stream == NULL) goto done;
    pa_stream_set_read_callback(stream, report_peak, loop);
    if (pa_stream_connect_record(stream, "ytmglass_null.monitor", &buffer,
                                 PA_STREAM_PEAK_DETECT | PA_STREAM_DONT_MOVE) < 0) goto done;
    while (PA_STREAM_IS_GOOD(pa_stream_get_state(stream))) {
        if (pa_mainloop_iterate(loop, 1, &status) < 0) break;
    }
    status = EXIT_FAILURE;
done:
    if (stream != NULL) pa_stream_unref(stream);
    if (context != NULL) pa_context_unref(context);
    pa_mainloop_free(loop);
    fputs("Null sink peak meter stopped\n", stderr);
    return status;
}
