# deotp

## Ideas
decentralized one time purchase with smart contract, serial keygen, application wrapper
smart contract- application owner generates a public contract to exchange x amount of value for a serial key sent back as a minted token
the generated key is cryptographically linked to the underlying serial check in the application wrapper
the application wrapper does not expose the serial to external applications
the application does not allow interaction, compute, visual elements, unless the serial is active
the wrapper does not need internet connection to verify active license key
the wrapper can run embedded rust applications and support tauri web applications 
the wrapper does not necessarilly interfere with the applications ability to access system files except external data but is still able to pause compute/kill the app 
the embedded apps are not able to run if the wrapper process is killed
the wrapper takes minimal overhead and extra cpu/memory 

## Architecture


## Implementation
