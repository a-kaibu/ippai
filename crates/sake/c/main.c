/* Entry point of sake.wasm: the program to run is argv[1] (a host path). */
extern int sake_main( int argc, char **argv );

int main( int argc, char *argv[] )
{
    return sake_main( argc, argv );
}
