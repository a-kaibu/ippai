/* Entry point of vnwin.wasm: the program to run is argv[1] (a host path). */
extern int vnwin_main( int argc, char **argv );

int main( int argc, char *argv[] )
{
    return vnwin_main( argc, argv );
}
