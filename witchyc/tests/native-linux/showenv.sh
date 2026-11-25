#!/bin/sh
# The variables a program was given (and that nothing else was; a shell sets its own PATH).
printf 'LANG=%s TZ=%s HOME=%s USER=%s\n' "${LANG-unset}" "${TZ-unset}" "${HOME-unset}" "${USER-unset}"
