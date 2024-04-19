cmake_minimum_required(VERSION 3.16)

project(msquic-asio)

if(WIN32)
  set(release_url "https://github.com/microsoft/msquic/releases/download/v2.3.5/msquic_windows_x64_Release_schannel.zip")
else()
  # linux
  set(release_url "https://github.com/microsoft/msquic/releases/download/v2.3.5/msquic_linux_x64_Release_openssl.zip")
  find_package(OpenSSL REQUIRED)
  # openssl needs version 1.1. ubuntu 20.04 has it, 22.04 is too new.
endif(WIN32)

message(STATUS "fetching msquic_release " ${release_url})
include(FetchContent)
FetchContent_Declare(
  msquic_release
  DOWNLOAD_EXTRACT_TIMESTAMP true
  URL ${release_url}
)

FetchContent_GetProperties(msquic_release)
if(NOT msquic_release_POPULATED)
  FetchContent_Populate(msquic_release)
endif()

if(WIN32)
# generate winmd. defer generation task in .metadata csproj.
find_program (
    dotnet_exe
    NAMES dotnet.exe
    REQUIRED
)

set(_out_winmds
  Microsoft.MsQuic.winmd
  Windows.Win32.winmd
)
set(_out_winmd_paths "")
foreach(_out_winmd ${_out_winmds})
list(APPEND _out_winmd_paths ${CMAKE_CURRENT_SOURCE_DIR}/.windows/winmd/${_out_winmd})
endforeach()

add_custom_command(
  OUTPUT ${_out_winmd_paths}
  COMMAND ${dotnet_exe} build
  WORKING_DIRECTORY ${CMAKE_CURRENT_SOURCE_DIR}/.metadata
  VERBATIM
)

add_custom_target(generate_winmd
  DEPENDS ${_out_winmd_paths}
)
else()
  file(GLOB _msquic_dll 
    ${msquic_release_SOURCE_DIR}/bin/libmsquic.so.*
  )
  # create a sym link
  execute_process(
    COMMAND ln -sf ${_msquic_dll} ${CMAKE_BINARY_DIR}/libmsquic.so # for linking
    COMMAND ln -sf ${_msquic_dll} ${CMAKE_BINARY_DIR}/libmsquic.so.2 # for loading at runtime
  )
endif(WIN32)