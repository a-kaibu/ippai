#pragma once
#include <vcruntime.h>
#include <eh.h>
extern "C" {
struct __std_exception_data { char const* _What; bool _DoFree; };
__declspec(dllimport) void __cdecl __std_exception_copy(__std_exception_data const*, __std_exception_data*);
__declspec(dllimport) void __cdecl __std_exception_destroy(__std_exception_data*);
}
namespace std {
class exception {
public:
    exception() noexcept : _Data() {}
    explicit exception(char const* const _Message) noexcept : _Data() { __std_exception_data _InitData = {_Message, true}; __std_exception_copy(&_InitData, &_Data); }
    exception(char const* const _Message, int) noexcept : _Data() { _Data._What = _Message; }
    exception(exception const& _Other) noexcept : _Data() { __std_exception_copy(&_Other._Data, &_Data); }
    exception& operator=(exception const& _Other) noexcept { if (this == &_Other) return *this; __std_exception_destroy(&_Data); __std_exception_copy(&_Other._Data, &_Data); return *this; }
    virtual ~exception() noexcept { __std_exception_destroy(&_Data); }
    virtual char const* what() const { return _Data._What ? _Data._What : "Unknown exception"; }
private:
    __std_exception_data _Data;
};
class bad_exception : public exception { public: bad_exception() noexcept : exception("bad exception", 1) {} };
class bad_alloc : public exception { public: bad_alloc() noexcept : exception("bad allocation", 1) {} private: friend class bad_array_new_length; bad_alloc(char const* const _Message) noexcept : exception(_Message, 1) {} };
class bad_array_new_length : public bad_alloc { public: bad_array_new_length() noexcept : bad_alloc("bad array new length") {} };
}
