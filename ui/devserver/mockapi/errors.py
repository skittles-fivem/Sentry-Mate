"""The one error shape: { error: { code, message, fields? } }."""

from __future__ import annotations


class ApiError(Exception):
    def __init__(self, status, code, message, fields=None, headers=None, extra=None):
        super().__init__(message)
        self.status = status
        self.code = code
        self.message = message
        self.fields = fields
        self.headers = headers or {}
        self.extra = extra or {}

    def body(self):
        e = {"code": self.code, "message": self.message}
        if self.fields:
            e["fields"] = self.fields
        e.update(self.extra)
        return {"error": e}


def bad_input(fields, message="Some values are not valid. See fields."):
    return ApiError(400, "bad_input", message, fields=fields)


def invalid(fields, message="Some values break a rule. See fields."):
    return ApiError(422, "invalid", message, fields=fields)


def not_signed_in():
    return ApiError(401, "not_signed_in", "Sign in first.")


def not_allowed(message="Your account may not do this."):
    return ApiError(403, "not_allowed", message)


def csrf():
    return ApiError(403, "csrf", "Changes need the header X-Sentry-Mate: 1.")


def no_such_car():
    return ApiError(404, "no_such_car", "There is no such car, or your account may not see it.")


def not_found(code="not_found", message="Nothing here."):
    return ApiError(404, code, message)


def stale_version(current):
    return ApiError(409, "stale_version", "Someone changed this since you loaded it. Load it again.",
                    extra={"version": current})


def conflict(code, message):
    return ApiError(409, code, message)


def throttled(retry_after_s):
    return ApiError(429, "throttled", f"Too many tries. Wait {retry_after_s} s.",
                    headers={"Retry-After": str(retry_after_s)})
