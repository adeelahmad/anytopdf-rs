//
// anytopdf-printer: an IPP Everywhere printer that turns print jobs into
// searchable PDFs.
//
// PAPPL receives each job and rasterizes it. This helper spools the pages as
// PWG Raster and hands the file to `anytopdf convert`, whose print-raster
// importer, OCR and renderer produce the PDF. The helper holds no format
// knowledge of its own.
//
// Copyright the anytopdf contributors. Licensed under MIT OR Apache-2.0.
//

#include <pappl/pappl.h>
#include <cups/raster.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <signal.h>
#include <spawn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

extern char **environ;

#if CUPS_VERSION_MAJOR < 3
#  define cupsRasterGetErrorString cupsRasterErrorString
#endif

#define PRINTER_NAME   "anytopdf"
#define DRIVER_NAME    "anytopdf"
#define DEFAULT_PORT   8631
#define DEFAULT_LISTEN "localhost"

#ifndef ANYTOPDF_PRINTER_VERSION
#  define ANYTOPDF_PRINTER_VERSION "0.1.0"
#endif

typedef struct
{
  char output_dir[PATH_MAX];  // Where finished PDFs are written
  char anytopdf[PATH_MAX];    // anytopdf executable (PATH lookup if bare)
  char state_file[PATH_MAX];  // Saved printer configuration
} app_config_t;

typedef struct
{
  int           fd;               // Spool file descriptor
  cups_raster_t *ras;             // PWG Raster writer
  char          path[PATH_MAX];   // Spool file path
} job_spool_t;

static app_config_t config;

static pappl_pr_driver_t drivers[] =
{
  { DRIVER_NAME, "anytopdf searchable PDF", NULL, NULL }
};

static const char *media[] =
{
  "na_letter_8.5x11in",
  "na_legal_8.5x14in",
  "iso_a4_210x297mm",
  "iso_a5_148x210mm",
  "iso_a3_297x420mm"
};


//
// 'option_or_env()' - Look up a "-o name=value" option, then an environment
//                     variable, then a default.
//

static const char *
option_or_env(int           num_options,
              cups_option_t *options,
              const char    *name,
              const char    *env,
              const char    *def)
{
  const char *value = cupsGetOption(name, num_options, options);

  if (!value && env)
    value = getenv(env);

  return (value && *value ? value : def);
}


//
// 'safe_name()' - Copy a job name into a file-name-safe form.
//

static void
safe_name(const char *name,
          char       *buffer,
          size_t     bufsize)
{
  char *ptr = buffer, *end = buffer + bufsize - 1;

  for (; name && *name && ptr < end; name ++)
  {
    unsigned char ch = (unsigned char)*name;

    if ((ch >= 'a' && ch <= 'z') || (ch >= 'A' && ch <= 'Z') || (ch >= '0' && ch <= '9') || ch == '-' || ch == '_' || ch == '.')
      *ptr++ = (char)ch;
    else if (ptr > buffer && ptr[-1] != '_')
      *ptr++ = '_';
  }

  while (ptr > buffer && (ptr[-1] == '_' || ptr[-1] == '.'))
    ptr --;

  *ptr = '\0';

  if (!buffer[0])
    snprintf(buffer, bufsize, "untitled");
}


//
// 'run_anytopdf()' - Convert a spooled raster file to a searchable PDF.
//

static bool
run_anytopdf(pappl_job_t *job,
             const char  *spool,
             const char  *output)
{
  pid_t pid;
  int   status, err;
  char  *argv[] = { config.anytopdf, "convert", (char *)spool, "--output", (char *)output, NULL };

  err = strchr(config.anytopdf, '/') ? posix_spawn(&pid, config.anytopdf, NULL, NULL, argv, environ)
                                     : posix_spawnp(&pid, config.anytopdf, NULL, NULL, argv, environ);
  if (err)
  {
    papplLogJob(job, PAPPL_LOGLEVEL_ERROR, "Unable to run '%s': %s", config.anytopdf, strerror(err));
    return (false);
  }

  while (waitpid(pid, &status, 0) < 0)
  {
    if (errno != EINTR)
    {
      papplLogJob(job, PAPPL_LOGLEVEL_ERROR, "Unable to wait for anytopdf: %s", strerror(errno));
      return (false);
    }
  }

  if (WIFEXITED(status) && WEXITSTATUS(status) == 0)
    return (true);

  if (WIFEXITED(status))
    papplLogJob(job, PAPPL_LOGLEVEL_ERROR, "anytopdf exited with status %d.", WEXITSTATUS(status));
  else
    papplLogJob(job, PAPPL_LOGLEVEL_ERROR, "anytopdf stopped by signal %d.", WTERMSIG(status));

  return (false);
}


//
// 'spool_close()' - Close and free a job's spool file.
//

static void
spool_close(job_spool_t *spool)
{
  if (spool->ras)
    cupsRasterClose(spool->ras);
  if (spool->fd >= 0)
    close(spool->fd);
}


//
// 'rstartjob_cb()' - Open the PWG Raster spool file for a job.
//

static bool
rstartjob_cb(pappl_job_t        *job,
             pappl_pr_options_t *options,
             pappl_device_t     *device)
{
  job_spool_t *spool = calloc(1, sizeof(job_spool_t));

  (void)options;
  (void)device;

  if (!spool)
    return (false);

  spool->fd = papplJobOpenFile(job, spool->path, sizeof(spool->path), NULL, "pwg", "w");
  if (spool->fd < 0)
  {
    papplLogJob(job, PAPPL_LOGLEVEL_ERROR, "Unable to create spool file: %s", strerror(errno));
    free(spool);
    return (false);
  }

  if ((spool->ras = cupsRasterOpen(spool->fd, CUPS_RASTER_WRITE_PWG)) == NULL)
  {
    papplLogJob(job, PAPPL_LOGLEVEL_ERROR, "Unable to open PWG Raster writer: %s", cupsRasterGetErrorString());
    spool_close(spool);
    unlink(spool->path);
    free(spool);
    return (false);
  }

  papplJobSetData(job, spool);
  return (true);
}


//
// 'rstartpage_cb()' - Write a page header.
//

static bool
rstartpage_cb(pappl_job_t        *job,
              pappl_pr_options_t *options,
              pappl_device_t     *device,
              unsigned           page)
{
  job_spool_t *spool = (job_spool_t *)papplJobGetData(job);

  (void)device;
  (void)page;

  return (spool && cupsRasterWriteHeader2(spool->ras, &options->header));
}


//
// 'rwriteline_cb()' - Write one line of pixels.
//

static bool
rwriteline_cb(pappl_job_t         *job,
              pappl_pr_options_t  *options,
              pappl_device_t      *device,
              unsigned            y,
              const unsigned char *line)
{
  job_spool_t *spool = (job_spool_t *)papplJobGetData(job);
  unsigned    bytes  = options->header.cupsBytesPerLine;

  (void)device;
  (void)y;

  return (spool && cupsRasterWritePixels(spool->ras, (unsigned char *)line, bytes) == bytes);
}


//
// 'rendpage_cb()' - Finish a page (nothing to do; pages are streamed).
//

static bool
rendpage_cb(pappl_job_t        *job,
            pappl_pr_options_t *options,
            pappl_device_t     *device,
            unsigned           page)
{
  (void)job;
  (void)options;
  (void)device;
  (void)page;

  return (true);
}


//
// 'rendjob_cb()' - Close the spool file and convert it.
//

static bool
rendjob_cb(pappl_job_t        *job,
           pappl_pr_options_t *options,
           pappl_device_t     *device)
{
  job_spool_t *spool = (job_spool_t *)papplJobGetData(job);
  char        name[128], stamp[32], output[PATH_MAX];
  time_t      now = time(NULL);
  struct tm   tm;
  bool        ok;

  (void)options;
  (void)device;

  if (!spool)
    return (false);

  spool_close(spool);
  papplJobSetData(job, NULL);

  safe_name(papplJobGetName(job), name, sizeof(name));
  localtime_r(&now, &tm);
  strftime(stamp, sizeof(stamp), "%Y%m%d-%H%M%S", &tm);
  if ((size_t)snprintf(output, sizeof(output), "%s/%s-job%d-%s.pdf", config.output_dir, stamp, papplJobGetID(job), name) >= sizeof(output))
  {
    papplLogJob(job, PAPPL_LOGLEVEL_ERROR, "Output path is too long.");
    unlink(spool->path);
    free(spool);
    return (false);
  }

  papplLogJob(job, PAPPL_LOGLEVEL_INFO, "Converting '%s' to '%s'.", spool->path, output);
  if ((ok = run_anytopdf(job, spool->path, output)) != false)
    papplJobSetMessage(job, "Saved %s", output);

  unlink(spool->path);
  free(spool);

  return (ok);
}


//
// 'status_cb()' - Report printer status (always idle and ready).
//

static bool
status_cb(pappl_printer_t *printer)
{
  (void)printer;

  return (true);
}


//
// 'driver_cb()' - Describe the anytopdf virtual printer.
//

static bool
driver_cb(pappl_system_t         *system,
          const char             *driver_name,
          const char             *device_uri,
          const char             *device_id,
          pappl_pr_driver_data_t *data,
          ipp_t                  **attrs,
          void                   *cbdata)
{
  size_t i;

  (void)device_uri;
  (void)device_id;
  (void)attrs;
  (void)cbdata;

  if (strcmp(driver_name, DRIVER_NAME))
  {
    papplLog(system, PAPPL_LOGLEVEL_ERROR, "Unknown driver '%s'.", driver_name);
    return (false);
  }

  data->rstartjob_cb  = rstartjob_cb;
  data->rstartpage_cb = rstartpage_cb;
  data->rwriteline_cb = rwriteline_cb;
  data->rendpage_cb   = rendpage_cb;
  data->rendjob_cb    = rendjob_cb;
  data->status_cb     = status_cb;

  papplCopyString(data->make_and_model, "anytopdf Searchable PDF", sizeof(data->make_and_model));
  data->ppm             = 60;
  data->ppm_color       = 60;
  data->kind            = PAPPL_KIND_DOCUMENT | PAPPL_KIND_PHOTO;
  data->color_supported = PAPPL_COLOR_MODE_AUTO | PAPPL_COLOR_MODE_COLOR | PAPPL_COLOR_MODE_MONOCHROME;
  data->color_default   = PAPPL_COLOR_MODE_AUTO;
  data->content_default = PAPPL_CONTENT_AUTO;
  data->quality_default = IPP_QUALITY_NORMAL;
  data->scaling_default = PAPPL_SCALING_AUTO;
  data->raster_types    = PAPPL_PWG_RASTER_TYPE_SGRAY_8 | PAPPL_PWG_RASTER_TYPE_SRGB_8;
  data->sides_supported = PAPPL_SIDES_ONE_SIDED;
  data->sides_default   = PAPPL_SIDES_ONE_SIDED;
  data->orient_default  = IPP_ORIENT_NONE;

  // 300 dpi is what OCR wants and what IPP Everywhere requires; higher
  // resolutions quadruple the work without improving recognition.
  data->num_resolution  = 1;
  data->x_resolution[0] = data->y_resolution[0] = 300;
  data->x_default       = data->y_default = 300;

  data->borderless = true;
  data->left_right = 0;
  data->bottom_top = 0;

  data->num_media = (int)(sizeof(media) / sizeof(media[0]));
  for (i = 0; i < sizeof(media) / sizeof(media[0]); i ++)
    data->media[i] = media[i];

  data->num_source = 1;
  data->source[0]  = "auto";

  papplCopyString(data->media_default.size_name, media[0], sizeof(data->media_default.size_name));
  data->media_default.size_width  = 21590;
  data->media_default.size_length = 27940;
  papplCopyString(data->media_default.source, "auto", sizeof(data->media_default.source));
  papplCopyString(data->media_default.type, "stationery", sizeof(data->media_default.type));
  data->media_ready[0] = data->media_default;

  data->num_type = 1;
  data->type[0]  = "stationery";

  return (true);
}


//
// 'save_cb()' - Persist the printer configuration.
//

static bool
save_cb(pappl_system_t *system,
        void           *data)
{
  (void)data;

  return (papplSystemSaveState(system, config.state_file));
}


//
// 'system_cb()' - Create the printer system for the "server" sub-command.
//
// Options (-o name=value) and environment fallbacks:
//
//   listen-hostname   ANYTOPDF_PRINTER_LISTEN  Interface to listen on ("localhost").
//   server-port       ANYTOPDF_PRINTER_PORT    IPP port (8631).
//   output-directory  ANYTOPDF_PRINTER_OUTPUT  Where PDFs are written (current directory).
//   anytopdf          ANYTOPDF_BIN             anytopdf executable ("anytopdf").
//   spool-directory   ANYTOPDF_PRINTER_SPOOL   Job spool and state (temporary directory).
//   log-file          -                        Log file ("-" for stderr).
//   log-level         -                        debug, info, warn, error or fatal.
//

static pappl_system_t *
system_cb(int           num_options,
          cups_option_t *options,
          void          *data)
{
  pappl_system_t   *system;
  pappl_loglevel_t loglevel = PAPPL_LOGLEVEL_INFO;
  const char       *listen, *value, *spooldir, *logfile;
  char             tmpdir[PATH_MAX];
  int              port;

  (void)data;

  listen   = option_or_env(num_options, options, "listen-hostname", "ANYTOPDF_PRINTER_LISTEN", DEFAULT_LISTEN);
  port     = atoi(option_or_env(num_options, options, "server-port", "ANYTOPDF_PRINTER_PORT", "8631"));
  logfile  = option_or_env(num_options, options, "log-file", NULL, "-");
  spooldir = option_or_env(num_options, options, "spool-directory", "ANYTOPDF_PRINTER_SPOOL", NULL);

  if (port <= 0 || port > 65535)
    port = DEFAULT_PORT;

  if (!spooldir)
  {
    snprintf(tmpdir, sizeof(tmpdir), "%s/anytopdf-printer-%u", papplGetTempDir(), (unsigned)getuid());
    if (mkdir(tmpdir, 0700) && errno != EEXIST)
    {
      fprintf(stderr, "anytopdf-printer: Unable to create '%s': %s\n", tmpdir, strerror(errno));
      return (NULL);
    }
    spooldir = tmpdir;
  }

  if ((value = cupsGetOption("log-level", num_options, options)) != NULL)
  {
    if (!strcmp(value, "debug"))
      loglevel = PAPPL_LOGLEVEL_DEBUG;
    else if (!strcmp(value, "warn"))
      loglevel = PAPPL_LOGLEVEL_WARN;
    else if (!strcmp(value, "error"))
      loglevel = PAPPL_LOGLEVEL_ERROR;
    else if (!strcmp(value, "fatal"))
      loglevel = PAPPL_LOGLEVEL_FATAL;
  }

  value = option_or_env(num_options, options, "output-directory", "ANYTOPDF_PRINTER_OUTPUT", ".");
  if (!realpath(value, config.output_dir))
  {
    fprintf(stderr, "anytopdf-printer: Output directory '%s': %s\n", value, strerror(errno));
    return (NULL);
  }

  papplCopyString(config.anytopdf, option_or_env(num_options, options, "anytopdf", "ANYTOPDF_BIN", "anytopdf"), sizeof(config.anytopdf));
  if ((size_t)snprintf(config.state_file, sizeof(config.state_file), "%s/anytopdf-printer.state", spooldir) >= sizeof(config.state_file))
  {
    fprintf(stderr, "anytopdf-printer: Spool directory path is too long.\n");
    return (NULL);
  }

  // Jobs come from the network, so TLS stays available and the web
  // interface only allows local administration.
  system = papplSystemCreate(PAPPL_SOPTIONS_WEB_INTERFACE | PAPPL_SOPTIONS_WEB_LOG, "anytopdf", port, "_print,_universal", spooldir, logfile, loglevel, NULL, false);
  if (!system)
    return (NULL);

  if (!papplSystemAddListeners(system, listen))
  {
    papplLog(system, PAPPL_LOGLEVEL_FATAL, "Unable to listen on '%s' port %d.", listen, port);
    papplSystemDelete(system);
    return (NULL);
  }

  papplSystemSetPrinterDrivers(system, (int)(sizeof(drivers) / sizeof(drivers[0])), drivers, NULL, NULL, driver_cb, NULL);
  papplSystemSetFooterHTML(system, "anytopdf searchable PDF printer");
  papplSystemSetSaveCallback(system, save_cb, NULL);

  if (!papplSystemLoadState(system, config.state_file) || !papplSystemGetDefaultPrinterID(system))
  {
    // First run: a single printer that needs no physical device.
    pappl_printer_t *printer = papplPrinterCreate(system, 0, PRINTER_NAME, DRIVER_NAME, "MFG:anytopdf;MDL:Searchable PDF;CMD:PWGRaster;", "file:///dev/null");

    if (!printer)
    {
      papplLog(system, PAPPL_LOGLEVEL_FATAL, "Unable to create the '%s' printer.", PRINTER_NAME);
      papplSystemDelete(system);
      return (NULL);
    }
    // Queue concurrent submissions instead of answering "busy".
    papplPrinterSetMaxActiveJobs(printer, 0);
    papplSystemSetDefaultPrinterID(system, papplPrinterGetID(printer));
  }

  papplLog(system, PAPPL_LOGLEVEL_INFO, "Writing PDFs to '%s' with '%s'.", config.output_dir, config.anytopdf);

  return (system);
}


//
// 'main()' - Run the printer application.
//

int
main(int  argc,
     char *argv[])
{
  // A converter that dies mid-job must not take the server down with it.
  signal(SIGPIPE, SIG_IGN);

  return (papplMainloop(argc, argv, ANYTOPDF_PRINTER_VERSION, NULL, (int)(sizeof(drivers) / sizeof(drivers[0])), drivers, NULL, driver_cb, NULL, NULL, system_cb, NULL, NULL));
}
